// RFC3339 UTC, 'Z' suffix, second precision — matches the mock fixtures'
// timestamp format exactly, so cursor comparisons can stay plain string
// comparison instead of parsing.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn find_github_url(text: &str) -> Option<String> {
    let idx = text.find("https://github.com/")?;
    let rest = &text[idx..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches(['.', ',']).to_string())
}

pub fn repo_from_url(url: &str) -> String {
    url.trim_start_matches("https://github.com/")
        .splitn(3, '/')
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
}

pub fn repo_display_name(repo: &str) -> String {
    let last = repo.rsplit('/').next().unwrap_or(repo);
    last.split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
