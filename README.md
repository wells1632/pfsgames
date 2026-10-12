# PFS Game History Downloader
This is a Rust package that will accept a Paizo username and password for the old website (not to be confused with the new Store website where actual financial information could be attained), login to
that website, then navigates to the Organized Play section of the site and downloads all of the session information for the account, dumping all of that information into a CSV file.

This should be useful for those that want to pull their data and organize it in an offline and more usable environment.

## Updates
- 12 October 2026:
    - Now will ask if you want to have CSV or XLS output
    - Cleaned up the output to eliminate blank lines and other artifacts
