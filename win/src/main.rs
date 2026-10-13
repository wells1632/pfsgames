// src/main.rs

// Launch only as a GUI app, with no console, on release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::mpsc;
use std::thread;
use std::sync::{Arc, Mutex};
use chrono::{Local, NaiveDate, Duration};
use rust_xlsxwriter::{Format, Workbook, XlsxColor};
use iced::keyboard;
use iced::keyboard::key::Named;
use iced::{event, Event};

// ----------------------------------------------------------------
// GUI Imports
// ----------------------------------------------------------------
use iced::{
    Alignment, Application, Command, Element, Settings, Theme,
    executor,
};

use iced::widget::{Button, TextInput, PickList, Container, Row, Column, Text, text_input};

// ----------------------------------------------------------------
// Scraper Logic Constants
// ----------------------------------------------------------------
const LOGIN_URL: &str = "https://paizo.com/cgi-bin/WebObjects/Store.woa/wa/DirectAction/signIn?path=organizedplay/myAccount";
const BASE_URL: &str = "https://paizo.com";

// ----------------------------------------------------------------
// Data Types
// ----------------------------------------------------------------
#[derive(Clone, Debug, PartialEq)]
enum OutputFormat {
    Csv,
    Xlsx,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 2] = [OutputFormat::Csv, OutputFormat::Xlsx];
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Csv => write!(f, "CSV"),
            OutputFormat::Xlsx => write!(f, "Excel (.xlsx)"),
        }
    }
}

// ----------------------------------------------------------------
// GUI Application State
// ----------------------------------------------------------------
#[derive(Clone, Debug, PartialEq)]
enum FocusedField {
    Email,
    Password,
    None,
}

struct ScraperGui {
    email_input: String,
    password_input: String,
    format: OutputFormat,
    status_log: String,
    is_working: bool,
    sender: Option<mpsc::Sender<String>>,
    receiver: Option<Arc<Mutex<mpsc::Receiver<String>>>>,
    focused_field: FocusedField,
    output_path: String,
}

#[derive(Clone, Debug)]
enum Message {
    EmailChanged(String),
    PasswordChanged(String),
    FormatChanged(OutputFormat),
    StartScraping,
    StatusUpdate(String),
    TabPressed,
    ChooseOutputPath,
    OutputPathChosen(Option<String>),
    OutputPathChanged(String),
}

impl ScraperGui {
    fn new() -> Self {
        Self {
            email_input: String::new(),
            password_input: String::new(),
            format: OutputFormat::Csv,
            status_log: String::from("Ready to start..."),
            is_working: false,
            sender: None,
            receiver: None,
            focused_field: FocusedField::None,
            output_path: String::new(),
        }
    }

    fn run_scraper(email: String, password: String, format: OutputFormat, output_path: String, sender: mpsc::Sender<String>) {
        thread::spawn(move || {
            let log = |msg: String| {
                let msg_clone = msg.clone();
                let _ = sender.send(msg);
                println!("{}", msg_clone);
            };

            // ----------------------------------------------------------------
            // Step 1: Login
            // ----------------------------------------------------------------
            log("Connecting to Paizo...".to_string());

            // Build client with cookie store
            let client = match Client::builder()
                .cookie_store(true)
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    log(format!("Error building client: {}", e));
                    return;
                }
            };

            // GET login page
            let _ = client.get("https://paizo.com/organizedPlay/myAccount").send();

            // POST credentials
            let form_fields = [
                ("path", "organizedplay/myAccount"),
                ("o", "v5748dmtz07mo"),
                ("p", ""),
                ("c", ""),
                ("e", &email),
                ("zzz", &password),
            ];

            let response = match client.post(LOGIN_URL)
                .header("Referer", "https://paizo.com/organizedPlay/myAccount")
                .header("Origin", "https://paizo.com")
                .form(&form_fields)
                .send()
            {
                Ok(r) => r,
                Err(e) => {
                    log(format!("POST Request failed: {}", e));
                    return;
                }
            };

            let landed_url = response.url().to_string();
            let body = match response.text() {
                Ok(b) => b,
                Err(e) => {
                    log(format!("Failed to read body: {}", e));
                    return;
                }
            };

            log(format!("Landed on: {}", landed_url));

            if landed_url.contains("signIn") || body.contains("Sign In") {
                log("Error: Login failed. Check credentials.".to_string());
                std::fs::write("debug_post_login.html", &body.as_bytes()).unwrap_or_default();
                return;
            }

            // ----------------------------------------------------------------
            // Step 2: Find Sessions Tab
            // ----------------------------------------------------------------
            let document = Html::parse_document(&body);
            let link_selector = Selector::parse("a").unwrap();

            let mut sessions_url: Option<String> = None;
            for link in document.select(&link_selector) {
                let text = link.text().collect::<String>();
                if text.trim().to_lowercase().contains("session") {
                    if let Some(href) = link.value().attr("href") {
                        let full_url = if href.starts_with("http") {
                            href.to_string()
                        } else {
                            format!("{}{}", BASE_URL, href)
                        };
                        sessions_url = Some(full_url);
                        break;
                    }
                }
            }

            let sessions_url = match sessions_url {
                Some(url) => url,
                None => {
                    log("Error: Could not find Sessions tab.".to_string());
                    std::fs::write("debug_post_login.html", &body.as_bytes()).unwrap_or_default();
                    return;
                }
            };

            log(format!("Sessions URL: {}", sessions_url));

            // ----------------------------------------------------------------
            // Step 3: Scraping Loop
            // ----------------------------------------------------------------
            let header_names = vec![
                "Date", "GM", "Scenario", "Points",
                "Event ID", "Event Name",
                "Session", "Player", "Character",
                "Faction", "Prest. / Rep.", "Notes",
            ];

            let mut all_rows: Vec<Vec<String>> = Vec::new();
            let mut current_url = sessions_url.clone();
            let mut page = 1;

            loop {
                log(format!("Fetching page {}...", page));
                let body = match client.get(&current_url).send() {
                    Ok(r) => r.text().unwrap_or_default(),
                    Err(_) => break, // Break on error
                };

                // Parse document
                let document = Html::parse_document(&body);
                let results_selector = Selector::parse("div#results").unwrap();
                let results_div = match document.select(&results_selector).next() {
                    Some(div) => div,
                    None => {
                        log(format!("No div#results found on page {}.", page));
                        std::fs::write(format!("debug_page_{}.html", page), &body.as_bytes()).unwrap_or_default();
                        break;
                    }
                };

                let table_selector = Selector::parse("table").unwrap();
                let mut tables = results_div.select(&table_selector);
                tables.next(); // skip pagination table
                let data_table = match tables.next() {
                    Some(t) => t,
                    None => {
                        log(format!("No data table found on page {}.", page));
                        break;
                    }
                };

                let row_selector = Selector::parse("tr").unwrap();
                let cell_selector = Selector::parse("td, th").unwrap();
                let expansion_selector = Selector::parse("a.expansion").unwrap();

                let mut page_rows = 0;
                for row in data_table.select(&row_selector) {
                    // Skip Show Seats rows
                    if row.select(&expansion_selector).next().is_some() {
                        continue;
                    }

                    let cells: Vec<String> = row
                        .select(&cell_selector)
                        .map(|cell| {
                            cell.text()
                                .collect::<String>()
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect();

                    // Skip blank / separator rows
                    if cells.iter().all(|c| c.trim().is_empty()) {
                        continue;
                    }

                    // Skip header rows
                    if cells.first().map(|c| c.trim() == "Date").unwrap_or(false) {
                        continue;
                    }

                    // Drop trailing "Edit" column
                    let cells: Vec<String> = cells
                        .into_iter()
                        .rev()
                        .skip_while(|c| c.trim() == "Edit")
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();

                    // Normalize date
                    let cells: Vec<String> = cells.into_iter().enumerate().map(|(i, val)| {
                        if i == 0 { normalize_date(&val) } else { val }
                    }).collect();

                    all_rows.push(cells);
                    page_rows += 1;
                }

                log(format!("Page {}: {} rows collected.", page, page_rows));

                // Find next link
                let link_selector = Selector::parse("a").unwrap();
                let next_href = results_div.select(&link_selector).find_map(|a| {
                    let text = a.text().collect::<String>().to_lowercase();
                    if text.contains("next") {
                        a.value().attr("href").map(|h| format!("https://paizo.com{}", h))
                    } else {
                        None
                    }
                });

                match next_href {
                    Some(url) => { current_url = url; page += 1; }
                    None => {
                        log("No next page link found. Scraping complete.".to_string());
                        break;
                    }
                }
            }

            // ----------------------------------------------------------------
            // Step 4: Write Output
            // ----------------------------------------------------------------

	    match format {
		OutputFormat::Csv => {
		    log("Writing CSV...".to_string());
		    let _ = write_csv_logic(&header_names, &all_rows, &output_path);
		    log(format!("Saved to {}", output_path));
		}
		OutputFormat::Xlsx => {
		    log("Writing XLSX...".to_string());
		    let _ = write_xlsx_logic(&header_names, &all_rows, &output_path);
		    log(format!("Saved to {}", output_path));
		}
	    }
	    
            // Signal end of work
            let _ = sender.send("DONE".to_string());
        });
    }
}

// ----------------------------------------------------------------
// GUI Implementation
// ----------------------------------------------------------------

impl Application for ScraperGui {
    type Executor = executor::Default;
    type Flags = ();
    type Message = Message;
    type Theme = Theme;

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self::new(), Command::none())
    }

    fn title(&self) -> String {
        String::from("Paizo Session Scraper")
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::EmailChanged(s) => self.email_input = s,
            Message::PasswordChanged(s) => self.password_input = s,
            Message::FormatChanged(f) => self.format = f,
	    Message::StartScraping => {
		if self.email_input.is_empty() || self.password_input.is_empty() {
		    self.status_log = "Error: Please enter email and password.".to_string();
		    return Command::none();
		}
		if self.output_path.is_empty() {
		    self.status_log = "Error: Please choose an output file path.".to_string();
		    return Command::none();
		}
		let (tx, rx) = mpsc::channel();
		let rx = Arc::new(Mutex::new(rx));
		self.sender = Some(tx);
		self.receiver = Some(rx.clone());
		self.status_log = "Starting...\n".to_string();
		self.is_working = true;

		let sender_clone = self.sender.clone().unwrap();
		let email = self.email_input.clone();
		let pass = self.password_input.clone();
		let fmt = self.format.clone();
		let output_path = self.output_path.clone();

		thread::spawn(move || {
		    ScraperGui::run_scraper(email, pass, fmt, output_path, sender_clone);
		});

		return Command::perform(
		    async move { rx.lock().unwrap().recv().unwrap_or("DONE".to_string()) },
		    Message::StatusUpdate,
		);
	    }

	    Message::StatusUpdate(s) => {
		if s == "DONE" {
		    self.is_working = false;
		    self.status_log.push_str("Done!\n");
		} else {
		    self.status_log.push_str(&s);
		    self.status_log.push('\n');

		    if let Some(rx) = self.receiver.clone() {
			return Command::perform(
			    async move { rx.lock().unwrap().recv().unwrap_or("DONE".to_string()) },
			    Message::StatusUpdate,
			);
		    }
		}
	    }
	    Message::TabPressed => {
		self.focused_field = match self.focused_field {
		    FocusedField::Email => FocusedField::Password,
		    _ => FocusedField::Email,
		};
		return match self.focused_field {
		    FocusedField::Password => text_input::focus(text_input::Id::new("password")),
		    _ => text_input::focus(text_input::Id::new("email")),
		};
	    }

	    Message::ChooseOutputPath => {
		let extension = match self.format {
		    OutputFormat::Csv => "csv",
		    OutputFormat::Xlsx => "xlsx",
		};
		return Command::perform(
		    async move {
			rfd::AsyncFileDialog::new()
			    .add_filter("Output file", &[extension])
			    .set_file_name(format!("sessions.{}", extension))
			    .save_file()
			    .await
			    .map(|f| f.path().to_string_lossy().to_string())
		    },
		    Message::OutputPathChosen,
		);
	    }
	    Message::OutputPathChosen(path) => {
		if let Some(p) = path {
		    self.output_path = p;
		}
	    }
	    Message::OutputPathChanged(s) => {
		self.output_path = s;
	    }
	    
        }

	
        Command::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
	let email_input = TextInput::<'_, Message, Theme, iced::Renderer>::new("Email", &self.email_input)
	    .id(text_input::Id::new("email"))
	    .on_input(Message::EmailChanged)
	    .on_submit(Message::TabPressed)
	    .padding(10);	
	let password_input: TextInput<'_, Message, Theme, iced::Renderer> = TextInput::new("Password", &self.password_input)
	    .id(text_input::Id::new("password"))
	    .on_input(Message::PasswordChanged)
//	    .on_submit(Message::StartScraping)
	    .padding(10)
	    .secure(true);
	
        // PickList::new takes 3 arguments: options, selected, and the on_change handler.
        // We map the Option<OutputFormat> to Message.
        let format_picker = PickList::new(
            &OutputFormat::ALL[..],
            Some(&self.format),
            |x| Message::FormatChanged(x)
        );

	let path_input = TextInput::<'_, Message, Theme, iced::Renderer>::new(
	    "Output file path...",
	    &self.output_path,
	)
	    .on_input(Message::OutputPathChanged)
	    .padding(10);

	let browse_btn = Button::new(Text::new("Browse..."))
	    .on_press(Message::ChooseOutputPath);

	let path_row = Row::new()
	    .push(path_input)
	    .push(browse_btn)
	    .spacing(10);

	
        let status_text = Text::new(self.status_log.as_str());

        let start_btn = {
            if self.is_working {
                Button::new(Text::new("Processing..."))
            } else {
                Button::new(Text::new("Start Scraping")).on_press(Message::StartScraping)
            }
        };

        let container = Container::new(
            Column::new()
                .push(
                    Row::new()
                        .push(email_input)
                        .push(password_input)
                )
                .push(format_picker)
		.push(path_row)
                .push(start_btn)
                .push(status_text)
                .spacing(20)
                .padding(20)
                .align_items(Alignment::Start)
        );
//	let email_id = text_input::Id::new("email");
//	let password_id = text_input::Id::new("password");


        container.into()
    }
    fn subscription(&self) -> iced::Subscription<Message> {
	event::listen_with(|event, _status| {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
		key: keyboard::Key::Named(Named::Tab),
		..
            }) = event
            {
		Some(Message::TabPressed)
            } else {
		None
            }
	})
    }
}

// ----------------------------------------------------------------
// Main Entry Point
// ----------------------------------------------------------------
fn main() -> iced::Result {
    ScraperGui::run(Settings::default())
}

// ----------------------------------------------------------------
// Logic Helpers
// ----------------------------------------------------------------
fn write_csv_logic(headers: &[&str], rows: &[Vec<String>], path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;

    let mut writer = BufWriter::new(file);

    let csv_header: Vec<String> = headers.iter().map(|h| format!("\"{}\"", h)).collect();
    writeln!(writer, "{}", csv_header.join(","))?;

    for row in rows {
        let csv_cells: Vec<String> = row.iter().map(|c| format!("\"{}\"", c.replace('"', "\"\""))).collect();
        writeln!(writer, "{}", csv_cells.join(","))?;
    }

    writer.flush()?;
    println!("Data saved to sessions.csv ({} rows)", rows.len());
    Ok(())
}

fn write_xlsx_logic(headers: &[&str], rows: &[Vec<String>], path: &str) -> Result<(), Box<dyn std::error::Error>> {

    // 1. Initialize Workbook with no arguments
    let mut workbook = Workbook::new(path);  

    // 2. Add worksheet without arguments and optionally set the name
    let sheet = workbook.add_worksheet();  
    sheet.set_name("Sessions")?;

    let header_format = Format::new()
        .set_bold()
        .set_font_color(XlsxColor::RGB(0x1F497D));  

    for (col, name) in headers.iter().enumerate() {
        // 3. Use type-specific format writer
        sheet.write_string(0, col as u16, *name, &header_format)?;
    }
    
    // 4. Use the correct freeze_panes method name
    // sheet.set_freeze_panes(1, 0)?;  

    for (row_idx, row) in rows.iter().enumerate() {
        let xlsx_row = (row_idx + 1) as u32;
        for (col_idx, cell) in row.iter().enumerate() {
            // 5. Use type-specific writer for strings
            sheet.write_string(xlsx_row, col_idx as u16, cell.as_str(), &Format::new())?;
        }
    }  

    let col_max_widths = [12u16, 20, 55, 45, 10, 30, 8, 14, 25, 18, 14, 20];
    for (col, max_w) in col_max_widths.iter().enumerate() {
        sheet.set_column_width(col as u16, *max_w as f64)?;
    }  

    // 6. Provide the filename to the save method
    workbook.close()?;
    println!("Data saved to sessions.xlsx ({} rows)", rows.len());
    Ok(())
}

fn normalize_date(date_str: &str) -> String {
    let today = Local::now().date_naive();
    let trimmed = date_str.trim();

    let weekdays = ["Monday","Tuesday","Wednesday","Thursday","Friday","Saturday","Sunday"];
    if weekdays.iter().any(|d| *d == trimmed) {
        for days_ago in 0..7 {
            let candidate = today - Duration::days(days_ago);
            let weekday_name = candidate.format("%A").to_string();
            if weekday_name == trimmed {
                return candidate.format("%d-%b-%y").to_string();
            }
        }
    }

    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%a, %b %e, %Y") {
        return d.format("%d-%b-%y").to_string();
    }

    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%a, %b %d, %Y") {
        return d.format("%d-%b-%y").to_string();
    }

    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%d-%b-%y") {
        return d.format("%d-%b-%y").to_string();
    }

    trimmed.to_string()
}
