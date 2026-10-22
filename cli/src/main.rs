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

/// Holds the parsed data for one character entry on the My Organized Play page.
struct CharacterRecord {
    number: String,           // e.g. "263598-1"
    game_version: String,     // e.g. "PFS(1ed)", "PFS(2ed)", "SFS"
    name: String,             // e.g. "Bobby Coltrane"
    factions: Vec<(String, String)>, // e.g. [("Dark Archive", "26"), ("Fame", "26")]
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

    let mut request_count: u32 = 0;

    // ----------------------------------------------------------------
    // Step 1: GET the login page to establish session cookies
    // ----------------------------------------------------------------
    client
        .get("https://paizo.com/organizedPlay/myAccount")
        .send()?;
    request_count += 1;

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
    request_count += 1;

    let landed_url = response.url().to_string();
    let body = response.text()?;
    println!("Landed on: {}", landed_url);

    if landed_url.contains("signIn") || body.contains("Sign In") {
        eprintln!("Login appears to have failed. Check your credentials.");
        std::fs::write("debug_post_login.html", &body)?;
        println!("Total HTTP requests made: {}", request_count);
        return Ok(());
    }

    // ----------------------------------------------------------------
    // NEW: Scrape character records from the My Organized Play page
    // ----------------------------------------------------------------
    let character_records = scrape_characters(&body);
    println!("Found {} character records.", character_records.len());

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
            println!("Total HTTP requests made: {}", request_count);
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
        request_count += 1;

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
    // Step 4b: Enrich rows that have bare player IDs (GM rows with
    //          "Show Seats") by looking up the full character ID from
    //          the character records we scraped in Step 2.
    //
    // The "Player" field is column 7 (0-indexed) and the "Character"
    // field is column 8.  Rows where the player ID has no hyphen are
    // the GM rows — we match on the character name to find the suffix.
    // ----------------------------------------------------------------
    // Build a name -> full character ID lookup from our character records.
    let name_to_char_id: std::collections::HashMap<String, String> = character_records
	.iter()
	.map(|r| (r.name.to_lowercase(), r.number.clone()))
	.collect();
    
    for row in all_rows.iter_mut() {
	// Column 7 = Player, Column 8 = Character name
	// Only act when the player field exists and has no hyphen.
	let needs_enrichment = row
            .get(7)
            .map(|p| !p.trim().is_empty() && !p.contains('-'))
            .unwrap_or(false);
	
	if !needs_enrichment {
            continue;
	}
	
	// Try to look up the character name (column 8).
	let char_name = row.get(8).cloned().unwrap_or_default();
	let key = char_name.trim().to_lowercase();
	
	if let Some(full_id) = name_to_char_id.get(&key) {
            if let Some(player_cell) = row.get_mut(7) {
		*player_cell = full_id.clone();
            }
	}
    }
    
    // ----------------------------------------------------------------
    // Step 5: Write output in the chosen format
    // ----------------------------------------------------------------
    match output_format {
        OutputFormat::Csv => write_csv(&header_names, &all_rows)?,
        OutputFormat::Xlsx => write_xlsx(&header_names, &all_rows, &character_records)?,
    }

    println!("Total HTTP requests made: {}", request_count);
    Ok(())
}

// ----------------------------------------------------------------
// Scrape character records from the My Organized Play landing page
// ----------------------------------------------------------------
fn scrape_characters(html: &str) -> Vec<CharacterRecord> {
    let document = Html::parse_document(html);
    let div_selector = Selector::parse("div").unwrap();

    // Find the one div whose normalised text contains ALL the characters.
    // It's the smallest (innermost) div that still has TOKENS == 54.
    // We pick the one with the fewest total characters, since inner divs
    // are subsets of outer ones.
    let mut best: Option<String> = None;
    for div in document.select(&div_selector) {
        let raw: String = div.text().collect::<Vec<_>>().join(" ");
        let norm: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if count_character_tokens(&norm) >= 1 {
            match &best {
                None => best = Some(norm),
                Some(prev) if norm.len() < prev.len() => best = Some(norm),
                _ => {}
            }
        }
    }

    let container_text = match best {
        Some(t) => t,
        None => return Vec::new(),
    };

    // Split the big text block on "# NNNNNN-N" boundaries.
    // We'll walk word-by-word and emit a new chunk each time we see "# <id>".
    let words: Vec<&str> = container_text.split_whitespace().collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < words.len() {
        if words[i] == "#" && i + 1 < words.len() && is_char_id(words[i + 1]) {
            if !current.is_empty() {
                chunks.push(current.join(" "));
            }
            current = vec![words[i], words[i + 1]];
            i += 2;
        } else {
            current.push(words[i]);
            i += 1;
        }
    }
    if !current.is_empty() {
        chunks.push(current.join(" "));
    }

    // Parse each chunk into a CharacterRecord.
    let mut records: Vec<CharacterRecord> = Vec::new();
    for chunk in &chunks {
        let words: Vec<&str> = chunk.split_whitespace().collect();

        // First two words must be "#" and "NNNNNN-N"
        if words.len() < 2 || words[0] != "#" || !is_char_id(words[1]) {
            continue;
        }
        let number = words[1].to_string();

        // Game version is the next whitespace token after the id
        // that matches a known game tag.
        let game_version = extract_game_version(chunk);
        if game_version.is_empty() {
            continue;
        }

        // Character name: the token(s) immediately after the game version tag,
        // up until we hit a faction keyword, "Edit", "Fame:", or "Total".
        let name = extract_name(chunk, &game_version);
        if name.is_empty() {
            continue;
        }

        let factions = extract_factions(chunk);

        records.push(CharacterRecord {
            number,
            game_version,
            name,
            factions,
        });
    }

    records
}

/// True if the token looks like "263598-1" or "263598-2001"
fn is_char_id(s: &str) -> bool {
    let mut parts = s.splitn(2, '-');
    let left  = parts.next().unwrap_or("");
    let right = parts.next().unwrap_or("");
    !left.is_empty()
        && !right.is_empty()
        && left.chars().all(|c| c.is_ascii_digit())
        && right.chars().all(|c| c.is_ascii_digit())
}

/// Extract the character name from a chunk.
/// The chunk looks like:
///   "# 263598-1 PFS(1ed) Bobby Coltrane Dark Archive: 26 Fame: 26 Edit Show Sessions ..."
/// We want "Bobby Coltrane".
fn extract_name(chunk: &str, game_version: &str) -> String {
    // Find where the game version token ends in the chunk, then collect
    // words until we hit a stop word / faction pattern.
    let stop_words = [
        "Edit", "Show", "Register", "AUC.register",
        "Fame:", "Total", "Deleted",
    ];
    // Known faction prefixes (first word of any "Faction Name: N" pair)
    let faction_starts = [
        "Envoy's", "Grand", "Horizon", "Radiant", "Verdant", "Vigilant",
        "Dark", "Silver", "Acquisitives", "Dataphiles", "Exo-Guardians",
        "Second", "Wayfinders", "Manifold",
    ];

    let words: Vec<&str> = chunk.split_whitespace().collect();

    // Find the index of the game_version token
    let version_words: Vec<&str> = game_version.split_whitespace().collect();
    let version_start = words.windows(version_words.len()).position(|w| w == version_words.as_slice());
    let name_start = match version_start {
        Some(pos) => pos + version_words.len(),
        None => return String::new(),
    };

    let mut name_words: Vec<&str> = Vec::new();
    for &word in &words[name_start..] {
        if stop_words.iter().any(|s| word.starts_with(s)) {
            break;
        }
        // A faction line looks like "FactionWord AnotherWord: N" —
        // stop if this word is a known faction start AND the next word
        // ends with ":"
        if faction_starts.iter().any(|s| word.starts_with(s)) {
            break;
        }
        name_words.push(word);
    }

    name_words.join(" ")
}


/// Returns the first "NNNNNN-NNN" token found after a "# " in the text, or None.
fn extract_character_number(text: &str) -> Option<String> {
    // Walk word-pairs looking for "#" followed by "NNNNNN-NNN"
    let words: Vec<&str> = text.split_whitespace().collect();
    for i in 0..words.len().saturating_sub(1) {
        if words[i] == "#" {
            let candidate = words[i + 1];
            // Must match digits-dash-digits, e.g. "263598-1" or "263598-2001"
            let is_char_id = {
                let mut parts = candidate.splitn(2, '-');
                let left  = parts.next().unwrap_or("");
                let right = parts.next().unwrap_or("");
                !left.is_empty()
                    && !right.is_empty()
                    && left.chars().all(|c| c.is_ascii_digit())
                    && right.chars().all(|c| c.is_ascii_digit())
            };
            if is_char_id {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// Counts how many "# NNNNNN-NNN" tokens appear in the text.
fn count_character_tokens(text: &str) -> usize {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut count = 0usize;
    for i in 0..words.len().saturating_sub(1) {
        if words[i] == "#" {
            let candidate = words[i + 1];
            let mut parts = candidate.splitn(2, '-');
            let left  = parts.next().unwrap_or("");
            let right = parts.next().unwrap_or("");
            if !left.is_empty()
                && !right.is_empty()
                && left.chars().all(|c| c.is_ascii_digit())
                && right.chars().all(|c| c.is_ascii_digit())
            {
                count += 1;
            }
        }
    }
    count
}

/// Extract the game version token from a block of text.
/// Looks for patterns like "PFS(1ed)", "PFS(2ed)", "SFS", etc.
fn extract_game_version(text: &str) -> String {
    // Order matters: longer/more-specific tokens first.
    let tokens = [
        "PFS(2ed)", "PFS(2nd)", "PFS (2nd)",
        "PFS(1ed)", "PFS(1st)", "PFS (1st)",
        "SFS2", "SFS",
        "ACS",
    ];
    for token in &tokens {
        if text.contains(token) {
            return token.to_string();
        }
    }
    // Fallback: grab the first word that starts with a known prefix.
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '(' && c != ')');
        if w.starts_with("PFS") || w.starts_with("SFS") || w.starts_with("ACS") {
            return w.to_string();
        }
    }
    String::new()
}

/// Extract faction name/value pairs (including Fame / Total Reputation) from block text.
fn extract_factions(text: &str) -> Vec<(String, String)> {
    let mut factions: Vec<(String, String)> = Vec::new();

    // Known faction names and scoring labels
    let faction_keys = [
        "Envoy's Alliance",
        "Grand Archive",
        "Horizon Hunters",
        "Radiant Oath",
        "Verdant Wheel",
        "Vigilant Seal",
        "Dark Archive",
        "Silver Crusade",
        "Grand Lodge",
        "Acquisitives",
        "Dataphiles",
        "Exo-Guardians",
        "Second Seekers",
        "Wayfinders",
        "Manifold Host",
        "Fame",
        "Total Reputation",
    ];

    // Normalize whitespace in the whole block so multi-line values collapse.
    let normalized: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    for key in &faction_keys {
        // Match "KeyName: VALUE" where VALUE is an integer
        let pattern = format!("{}: ", key);
        if let Some(pos) = normalized.find(&pattern) {
            let after = &normalized[pos + pattern.len()..];
            // Grab the leading integer token
            let value: String = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !value.is_empty() {
                factions.push((key.to_string(), value));
            }
        }
    }

    factions
}

// ----------------------------------------------------------------
// CSV output  (unchanged)
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
// XLSX output  — now accepts character_records for the second tab
// ----------------------------------------------------------------
fn write_xlsx(
    headers: &[&str],
    rows: &[Vec<String>],
    character_records: &[CharacterRecord],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut workbook = Workbook::new();

    // ── Header format (shared) ──────────────────────────────────────
    let header_format = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F497D))
        .set_border_bottom(rust_xlsxwriter::FormatBorder::Thin);

    // ================================================================
    // Sheet 1: Sessions  (existing logic, unchanged)
    // ================================================================
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("Sessions")?;

        for (col, name) in headers.iter().enumerate() {
            sheet.write_with_format(0, col as u16, *name, &header_format)?;
        }
        sheet.set_freeze_panes(1, 0)?;

        for (row_idx, row) in rows.iter().enumerate() {
            let xlsx_row = (row_idx + 1) as u32;
            for (col_idx, cell) in row.iter().enumerate() {
                sheet.write(xlsx_row, col_idx as u16, cell.as_str())?;
            }
        }

        let col_max_widths = [12u16, 20, 55, 45, 10, 30, 8, 14, 25, 18, 14, 20];
        for (col, max_w) in col_max_widths.iter().enumerate() {
            sheet.set_column_width(col as u16, *max_w as f64)?;
        }
    }

    // ================================================================
    // Sheet 2: Characters
    //
    // Columns:
    //   A: Character #
    //   B: Game Version
    //   C: Name
    //   D: Faction 1
    //   E: Points 1
    //   F: Faction 2
    //   G: Points 2
    //   … (pairs continue for however many factions exist)
    //
    // Because different game versions have different faction sets, and
    // the number of factions varies per character, we use a flat
    // "Faction N / Points N" column layout.  Find the maximum faction
    // count across all characters first so we can write headers.
    // ================================================================
// ================================================================
// Sheet 2: Characters
// ================================================================
    {
	let sheet = workbook.add_worksheet();
	sheet.set_name("Characters")?;
	
	// ── Pass 1: collect every faction name that appears across all records.
	// Use an IndexSet-style approach: preserve insertion order after sorting,
	// but pin Fame / Total Reputation to the front.
	let mut faction_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
	for rec in character_records {
            for (faction_name, _) in &rec.factions {
		// Don't add Fame/Total Reputation here — we'll pin them manually.
		if faction_name != "Fame" && faction_name != "Total Reputation" {
                    faction_set.insert(faction_name.clone());
		}
            }
	}
	
	// Build the ordered faction column list:
	//   Fame first, then Total Reputation (if present), then all others A-Z.
	let has_fame = character_records
            .iter()
            .any(|r| r.factions.iter().any(|(f, _)| f == "Fame"));
	let has_total_rep = character_records
            .iter()
            .any(|r| r.factions.iter().any(|(f, _)| f == "Total Reputation"));

	let mut faction_columns: Vec<String> = Vec::new();
	if has_fame        { faction_columns.push("Fame".to_string()); }
	if has_total_rep   { faction_columns.push("Total Reputation".to_string()); }
	faction_columns.extend(faction_set.into_iter()); // BTreeSet gives A-Z order

	// ── Write header row ──────────────────────────────────────────
	sheet.write_with_format(0, 0, "Character #",  &header_format)?;
	sheet.write_with_format(0, 1, "Game Version", &header_format)?;
	sheet.write_with_format(0, 2, "Name",         &header_format)?;
	for (i, faction_name) in faction_columns.iter().enumerate() {
            sheet.write_with_format(0, (3 + i) as u16, faction_name.as_str(), &header_format)?;
	}

	sheet.set_freeze_panes(1, 0)?;

	// ── Write data rows ───────────────────────────────────────────
	for (row_idx, rec) in character_records.iter().enumerate() {
            let xlsx_row = (row_idx + 1) as u32;
            sheet.write(xlsx_row, 0, rec.number.as_str())?;
            sheet.write(xlsx_row, 1, rec.game_version.as_str())?;
            sheet.write(xlsx_row, 2, rec.name.as_str())?;

            // Build a quick lookup for this character's faction scores.
            let faction_map: std::collections::HashMap<&str, &str> = rec
		.factions
		.iter()
		.map(|(k, v)| (k.as_str(), v.as_str()))
		.collect();

            for (i, faction_name) in faction_columns.iter().enumerate() {
		if let Some(points) = faction_map.get(faction_name.as_str()) {
                    // Write as a number if possible, otherwise as text.
                    if let Ok(n) = points.parse::<u32>() {
			sheet.write_number(xlsx_row, (3 + i) as u16, n as f64)?;
                    } else {
			sheet.write(xlsx_row, (3 + i) as u16, *points)?;
                    }
		}
		// If the character has no entry for this faction, leave the cell blank.
            }
	}

	// ── Column widths ─────────────────────────────────────────────
	sheet.set_column_width(0, 14.0)?; // Character #
	sheet.set_column_width(1, 14.0)?; // Game Version
	sheet.set_column_width(2, 28.0)?; // Name
	for i in 0..faction_columns.len() {
            // Faction name columns — wide enough for the longest name
            let w = (faction_columns[i].len() as f64 + 2.0).max(10.0);
            sheet.set_column_width((3 + i) as u16, w)?;
	}
    }

    workbook.save("sessions.xlsx")?;
    println!(
        "Data saved to sessions.xlsx ({} session rows, {} characters)",
        rows.len(),
        character_records.len()
    );
    Ok(())
}

// ----------------------------------------------------------------
// Helper: normalize date strings to dd-mmm-yy  (unchanged)
// ----------------------------------------------------------------
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
