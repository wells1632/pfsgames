use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use chrono::{Local, NaiveDate, Duration};
use rust_xlsxwriter::{Format, Workbook, Color};

const LOGIN_URL: &str = "https://paizo.com/cgi-bin/WebObjects/Store.woa/wa/DirectAction/signIn?path=organizedplay/myAccount";
const BASE_URL: &str = "https://paizo.com";

#[derive(PartialEq)]
enum OutputFormat {
    Csv,
    Xlsx,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    print!("Output format [csv/xlsx] (default: csv): ");
    io::stdout().flush()?;
    let mut fmt_input = String::new();
    io::stdin().read_line(&mut fmt_input)?;
    let output_format = match fmt_input.trim().to_lowercase().as_str() {
        "xlsx" => OutputFormat::Xlsx,
        _ => OutputFormat::Csv,
    };

    print!("Email: ");
    io::stdout().flush()?;
    let mut email = String::new();
    io::stdin().read_line(&mut email)?;
    let email = email.trim().to_string();
    let password = rpassword::prompt_password("Password: ")?;

    let client = Client::builder()
        .cookie_store(true)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    let mut request_count: u32 = 0; // +++ ADDED

    // ----------------------------------------------------------------
    // Step 1: GET the login page to establish session cookies
    // ----------------------------------------------------------------
    client
        .get("https://paizo.com/organizedPlay/myAccount")
        .send()?;
    request_count += 1; // +++ ADDED

    // ----------------------------------------------------------------
    // Step 2: POST credentials
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
    request_count += 1; // +++ ADDED

    let landed_url = response.url().to_string();
    let body = response.text()?;
    println!("Landed on: {}", landed_url);

    if landed_url.contains("signIn") || body.contains("Sign In") {
        eprintln!("Login appears to have failed. Check your credentials.");
        std::fs::write("debug_post_login.html", &body)?;
        println!("Total HTTP requests made: {}", request_count); // +++ ADDED
        return Ok(());
    }

    // ----------------------------------------------------------------
    // Step 3: Find the Sessions tab URL
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
            println!("Total HTTP requests made: {}", request_count); // +++ ADDED
            return Ok(());
        }
    };

    println!("Sessions URL: {}", sessions_url);

    // ----------------------------------------------------------------
    // Step 4: Collect all rows across all pages
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
        println!("Fetching page {}...", page);
        let body = client.get(&current_url).send()?.text()?;
        request_count += 1; // +++ ADDED

        let document = Html::parse_document(&body);

        let results_selector = Selector::parse("div#results").unwrap();
        let results_div = match document.select(&results_selector).next() {
            Some(div) => div,
            None => {
                println!("No div#results found on page {}. Stopping.", page);
                std::fs::write(format!("debug_page_{}.html", page), &body)?;
                break;
            }
        };

        let table_selector = Selector::parse("table").unwrap();
        let mut tables = results_div.select(&table_selector);
        tables.next();
        let data_table = match tables.next() {
            Some(t) => t,
            None => {
                println!("No data table found on page {}. Stopping.", page);
                break;
            }
        };

        let row_selector = Selector::parse("tr").unwrap();
        let cell_selector = Selector::parse("td, th").unwrap();
        let expansion_selector = Selector::parse("a.expansion").unwrap();

        let mut page_rows = 0;
        for row in data_table.select(&row_selector) {
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

            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }

            if cells.first().map(|c| c.trim() == "Date").unwrap_or(false) {
                continue;
            }

            let cells: Vec<String> = cells
                .into_iter()
                .rev()
                .skip_while(|c| c.trim() == "Edit")
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            let cells: Vec<String> = cells
                .into_iter()
                .enumerate()
                .map(|(i, val)| if i == 0 { normalize_date(&val) } else { val })
                .collect();

            all_rows.push(cells);
            page_rows += 1;
        }

        println!("Page {}: {} rows collected.", page, page_rows);

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
            None => { println!("No next page link found. Done."); break; }
        }
    }

    // ----------------------------------------------------------------
    // Step 5: Write output in the chosen format
    // ----------------------------------------------------------------
    match output_format {
        OutputFormat::Csv => write_csv(&header_names, &all_rows)?,
        OutputFormat::Xlsx => write_xlsx(&header_names, &all_rows)?,
    }

    println!("Total HTTP requests made: {}", request_count); // +++ ADDED
    Ok(())
}
// ----------------------------------------------------------------
// CSV output
// ----------------------------------------------------------------
fn write_csv(
    headers: &[&str],
    rows: &[Vec<String>],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create("sessions.csv")?;
    let mut writer = BufWriter::new(file);

    let csv_header: Vec<String> = headers
        .iter()
        .map(|h| format!("\"{}\"", h))
        .collect();
    writeln!(writer, "{}", csv_header.join(","))?;

    for row in rows {
        let csv_cells: Vec<String> = row
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect();
        writeln!(writer, "{}", csv_cells.join(","))?;
    }

    writer.flush()?;
    println!("Data saved to sessions.csv ({} rows)", rows.len());
    Ok(())
}

// ----------------------------------------------------------------
// XLSX output
// ----------------------------------------------------------------
fn write_xlsx(
    headers: &[&str],
    rows: &[Vec<String>],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Sessions")?;

    // Header format: bold white text on a dark blue background
    let header_format = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F497D))
        .set_border_bottom(rust_xlsxwriter::FormatBorder::Thin);

    // Write header row
    for (col, name) in headers.iter().enumerate() {
        sheet.write_with_format(0, col as u16, *name, &header_format)?;
    }

    // Freeze the header row so it stays visible when scrolling
    sheet.set_freeze_panes(1, 0)?;

    // Write data rows
    for (row_idx, row) in rows.iter().enumerate() {
        let xlsx_row = (row_idx + 1) as u32;
        for (col_idx, cell) in row.iter().enumerate() {
            sheet.write(xlsx_row, col_idx as u16, cell.as_str())?;
        }
    }

    // Auto-fit column widths (capped so Scenario/Points don't go wild)
    let col_max_widths = [12u16, 20, 55, 45, 10, 30, 8, 14, 25, 18, 14, 20];
    for (col, max_w) in col_max_widths.iter().enumerate() {
        sheet.set_column_width(col as u16, *max_w as f64)?;
    }

    workbook.save("sessions.xlsx")?;
    println!("Data saved to sessions.xlsx ({} rows)", rows.len());
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
        for days_ago in 0..7 {
            let candidate = today - Duration::days(days_ago);
            let weekday_name = candidate.format("%A").to_string();
            if weekday_name == trimmed {
                return candidate.format("%d-%b-%y").to_string();
            }
        }
    }

    // Format 2: "Sat, Oct 3, 2026" (single-digit day from <time> element)
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%a, %b %e, %Y") {
        return d.format("%d-%b-%y").to_string();
    }

    // Format 3: "Sat, Mar 15, 2025" (two-digit day)
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%a, %b %d, %Y") {
        return d.format("%d-%b-%y").to_string();
    }

    // Format 4: Already in "15-Mar-25" form — return as-is
    if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%d-%b-%y") {
        return d.format("%d-%b-%y").to_string();
    }

    // Fallback
    trimmed.to_string()
}

// ----------------------------------------------------------------
// Tests
// ----------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_long_date_zero_padded() {
        assert_eq!(normalize_date("Sat, Mar 15, 2025"), "15-Mar-25");
    }

    #[test]
    fn test_long_date_single_digit() {
        assert_eq!(normalize_date("Sat, Oct 3, 2026"), "03-Oct-26");
    }

    #[test]
    fn test_already_normalized() {
        assert_eq!(normalize_date("03-Oct-26"), "03-Oct-26");
    }

    #[test]
    fn test_fallback_unknown() {
        assert_eq!(normalize_date("  some garbage  "), "some garbage");
    }

    #[test]
    fn test_whitespace_trimming() {
        assert_eq!(normalize_date("  Sat, Oct 3, 2026  "), "03-Oct-26");
    }
}
