use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use chrono::{Local, NaiveDate, Datelike, Duration};

const LOGIN_URL: &str = "https://paizo.com/cgi-bin/WebObjects/Store.woa/wa/DirectAction/signIn?path=organizedplay/myAccount";
const BASE_URL: &str = "https://paizo.com";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Prompt for credentials
    print!("Email: ");
    io::stdout().flush()?;
    let mut email = String::new();
    io::stdin().read_line(&mut email)?;
    let email = email.trim().to_string();
    let password = rpassword::prompt_password("Password: ")?;

    // Build client with cookie store
    let client = Client::builder()
        .cookie_store(true)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    // ----------------------------------------------------------------
    // Step 1: GET the login page first to establish session cookies
    // ----------------------------------------------------------------
    client
        .get("https://paizo.com/organizedPlay/myAccount")
        .send()?;

    // ----------------------------------------------------------------
    // Step 2: POST credentials using the correct field names
    // ----------------------------------------------------------------
    let form_fields = [
        ("path", "organizedplay/myAccount"),
        ("o", "v5748dmtz07mo"),
        ("p", ""),
        ("c", ""),
        ("e", &email),
        ("zzz", &password),
    ];

    let response = client
        .post(LOGIN_URL)
        .header("Referer", "https://paizo.com/organizedPlay/myAccount")
        .header("Origin", "https://paizo.com")
        .form(&form_fields)
        .send()?;

    let landed_url = response.url().to_string();
    let body = response.text()?;
    println!("Landed on: {}", landed_url);

    // Check if login failed (i.e. we're back on the login page)
    if landed_url.contains("signIn") || body.contains("Sign In") {
        eprintln!("Login appears to have failed. Check your credentials.");
        std::fs::write("debug_post_login.html", &body)?;
        return Ok(());
    }

    // ----------------------------------------------------------------
    // Step 3: Find and navigate to the "Sessions" tab
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
            eprintln!("Could not find Sessions tab. Saving page to debug_post_login.html.");
            std::fs::write("debug_post_login.html", &body)?;
            return Ok(());
        }
    };

    println!("Sessions URL: {}", sessions_url);

    // ----------------------------------------------------------------
    // Step 4: Paginate through the sessions table
    // ----------------------------------------------------------------
    let output_file = File::create("sessions.csv")?;
    let mut writer = BufWriter::new(output_file);
    let mut headers_written = false;

    let mut current_url = sessions_url.clone();
    let mut page = 1;

    loop {
        println!("Fetching page {}...", page);
        let body = client.get(&current_url).send()?.text()?;
        let document = Html::parse_document(&body);

        // Scope to div#results
        let results_selector = Selector::parse("div#results").unwrap();
        let results_div = match document.select(&results_selector).next() {
            Some(div) => div,
            None => {
                println!("No div#results found on page {}. Stopping.", page);
                std::fs::write(format!("debug_page_{}.html", page), &body)?;
                break;
            }
        };

        // Skip the pagination table, grab the data table
        let table_selector = Selector::parse("table").unwrap();
        let mut tables = results_div.select(&table_selector);
        tables.next(); // skip pagination table
        let data_table = match tables.next() {
            Some(t) => t,
            None => {
                println!("No data table found on page {}. Stopping.", page);
                break;
            }
        };

        let row_selector = Selector::parse("tr").unwrap();
        let cell_selector = Selector::parse("td, th").unwrap();

        let mut data_rows_found = 0;
        for row in data_table.select(&row_selector) {
            let cells: Vec<String> = row
                .select(&cell_selector)
                .map(|cell| cell.text().collect::<String>().trim().to_string())
                .collect();

            if cells.is_empty() {
                continue;
            }

            // Discard "Show Seats" rows
            if cells.len() == 1 && cells[0].trim() == "Show Seats" {
                continue;
            }
            // Discard "Show Seats" rows
            if cells.len() == 1 && cells[0].trim() == "Show" {
                continue;
            }

            // Drop the last column ("Edit")
            let cells: Vec<String> = cells
                .into_iter()
                .rev()
                .skip_while(|c| c.trim() == "Edit")
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            // Normalize the date in the first column
            let cells: Vec<String> = cells
                .into_iter()
                .enumerate()
                .map(|(i, val)| {
                    if i == 0 {
                        normalize_date(&val)
                    } else {
                        val
                    }
                })
                .collect();

            // CSV-escape all values
            let csv_cells: Vec<String> = cells
                .iter()
                .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                .collect();

            // Write header only on first page
            if !headers_written {
                writeln!(writer, "{}", csv_cells.join(","))?;
                headers_written = true;
                continue;
            }

            // Skip header rows on subsequent pages (first row of each page)
            if data_rows_found == 0 && page > 1 {
                data_rows_found += 1;
                continue;
            }

            writeln!(writer, "{}", csv_cells.join(","))?;
            data_rows_found += 1;
        }

        writer.flush()?;
        println!("Page {}: {} rows written.", page, data_rows_found);

        // Follow the "next >" href directly
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
            Some(url) => {
                current_url = url;
                page += 1;
            }
            None => {
                println!("No next page link found. Done.");
                break;
            }
        }
    }

    println!("Data saved to sessions.csv");
    Ok(())
}

// ----------------------------------------------------------------
    // Helper: normalize date strings to dd-mmm-yy
    // ----------------------------------------------------------------
    fn normalize_date(date_str: &str) -> String {
        let today = Local::now().date_naive();
        let trimmed = date_str.trim();

        // Format 1: "Saturday" / "Monday" etc. (within last week)
        let weekdays = ["Monday","Tuesday","Wednesday","Thursday","Friday","Saturday","Sunday"];
        if weekdays.iter().any(|d| *d == trimmed) {
            // Walk back up to 7 days to find matching weekday
            for days_ago in 0..7 {
                let candidate = today - Duration::days(days_ago);
                let weekday_name = candidate.format("%A").to_string();
                if weekday_name == trimmed {
                    return candidate.format("%d-%b-%y").to_string();
                }
            }
        }

        // Format 2: "Sat, Mar 15, 2025" (within last month)
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%a, %b %d, %Y") {
            return d.format("%d-%b-%y").to_string();
        }

        // Format 3: Already in "15-Mar-25" form — return as-is
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%d-%b-%y") {
            return d.format("%d-%b-%y").to_string();
        }

        // Fallback: return original if we can't parse it
        trimmed.to_string()
    }
