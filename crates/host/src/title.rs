//! Automatic session titles.
//!
//! A session the user hasn't named is named by the host, so every client
//! shows the same name. In order of preference:
//!
//! 1. the title the agent gives the session itself (ACP `session_info_update`);
//! 2. a title the agent writes when asked, in a hidden conversation of its
//!    own, from the session's first message;
//! 3. until one of those arrives, the first message itself, shortened.
//!
//! A title the user gives a session (`session.rename`) always wins over them.

use agenthost_proto::SessionConfigOption;

/// Where an automatic title came from. A title only replaces one of the same
/// or a lower rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TitleSource {
    /// The first message, shortened.
    Prompt,
    /// Written by the agent when the host asked for one.
    Asked,
    /// Given by the agent on its own.
    Agent,
}

impl TitleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleSource::Prompt => "prompt",
            TitleSource::Asked => "asked",
            TitleSource::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prompt" => Some(TitleSource::Prompt),
            "asked" => Some(TitleSource::Asked),
            "agent" => Some(TitleSource::Agent),
            _ => None,
        }
    }
}

/// Longest automatic title, in characters (shorter than a title the user
/// may type, so it fits a list row).
pub const MAX_AUTO_TITLE: usize = 60;

/// How much of the first message the agent is shown when asked for a title.
const MAX_REQUEST: usize = 2000;

/// The start of the message that asks the agent for a title.
pub const ASK_PREFIX: &str = "Name this conversation.";

/// A title from the first message: its first line of prose, without markdown,
/// cut to its first sentence and to [`MAX_AUTO_TITLE`] characters.
pub fn from_prompt(text: &str) -> Option<String> {
    let mut in_fence = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("```") || l.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let l = l.trim_start_matches(['#', '>', '*', '-', '+', ' ']);
        let l: String = l.chars().filter(|c| !matches!(c, '`' | '*' | '_')).collect();
        let l = collapse_spaces(&l);
        if l.chars().any(char::is_alphanumeric) {
            return Some(shorten(&capitalize(first_sentence(&l)), MAX_AUTO_TITLE));
        }
    }
    None
}

/// The message asking the agent for a title for a conversation that starts
/// with `request`.
pub fn ask(request: &str) -> String {
    let request: String = request.chars().take(MAX_REQUEST).collect();
    format!(
        "{ASK_PREFIX} Reply with only a short title for it: 2 to 6 words, in sentence case, \
         in the language of the request, with no quotes and no full stop. Do not use any \
         tools and do not do the task.\n\n<request>\n{}\n</request>",
        request.trim()
    )
}

/// The title in the agent's answer to [`ask`], or `None` if it didn't give one.
pub fn from_answer(answer: &str) -> Option<String> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_start_matches(['#', '>', '*', '-', ' ']);
    let line = strip_label(line);
    let line: String = line.chars().filter(|c| !matches!(c, '`' | '*' | '_')).collect();
    let line = collapse_spaces(&line);
    let line = line
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | ' '))
        .trim_end_matches(['.', '!', ' '])
        .trim();
    if !line.chars().any(char::is_alphanumeric) || line.chars().count() > MAX_AUTO_TITLE * 2 {
        return None;
    }
    Some(shorten(&capitalize(line), MAX_AUTO_TITLE))
}

/// A title the agent gave the session itself, tidied.
pub fn from_agent(title: &str) -> Option<String> {
    let t = collapse_spaces(title);
    (!t.is_empty()).then(|| shorten(&t, agenthost_proto::MAX_SESSION_TITLE))
}

/// The agent's quickest model, for writing a title: `(config id, value)`.
/// `None` when it offers no model setting or none that looks quick.
pub fn quick_model(options: &[SessionConfigOption]) -> Option<(String, String)> {
    let opt = options.iter().find(|o| o.category.as_deref() == Some("model") || o.id == "model")?;
    const QUICK: [&str; 4] = ["haiku", "flash", "mini", "fast"];
    let v = QUICK.iter().find_map(|q| {
        opt.options.iter().find(|v| {
            v.value.to_lowercase().contains(q) || v.name.to_lowercase().contains(q)
        })
    })?;
    (v.value != opt.current_value).then(|| (opt.id.clone(), v.value.clone()))
}

fn strip_label(s: &str) -> &str {
    for label in ["title:", "session title:", "conversation title:"] {
        if s.get(..label.len()).is_some_and(|h| h.eq_ignore_ascii_case(label)) {
            return s[label.len()..].trim_start();
        }
    }
    s
}

fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Up to the end of the first sentence (a `.`, `!` or `?` followed by a
/// space), keeping a question mark but not a full stop. Very short
/// "sentences" are kept going: they are more likely "e.g." or "Hi.".
fn first_sentence(s: &str) -> &str {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if matches!(c, b'.' | b'!' | b'?') && b.get(i + 1) == Some(&b' ') && i >= 15 {
            return if c == b'?' { &s[..=i] } else { &s[..i] };
        }
    }
    s.strip_suffix('.').unwrap_or(s)
}

/// At most `max` characters, cut at a word where there is one, with "…".
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max - 1).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => &cut[..i],
        _ => &cut[..],
    };
    format!("{}…", cut.trim_end_matches([' ', ',', ';', ':', '.', '-']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenthost_proto::SessionConfigValue;

    #[test]
    fn a_prompt_becomes_its_first_sentence() {
        assert_eq!(from_prompt("fix the flaky resume test").as_deref(), Some("Fix the flaky resume test"));
        assert_eq!(
            from_prompt("The resume test is flaky. Can you look at it and fix it?").as_deref(),
            Some("The resume test is flaky")
        );
        assert_eq!(from_prompt("why does `cargo test` hang? it used to work").as_deref(), Some("Why does cargo test hang?"));
        assert_eq!(from_prompt("## Add dark mode\n\nto the settings").as_deref(), Some("Add dark mode"));
        assert_eq!(from_prompt("e.g. this").as_deref(), Some("E.g. this"));
    }

    #[test]
    fn code_and_blank_lines_are_skipped() {
        assert_eq!(
            from_prompt("\n```\npanic at main.rs:3\n```\nwhat does this panic mean").as_deref(),
            Some("What does this panic mean")
        );
        assert_eq!(from_prompt("   \n```\nonly code\n```"), None);
        assert_eq!(from_prompt("---"), None);
    }

    #[test]
    fn long_prompts_are_cut_at_a_word() {
        let t = from_prompt(&"refactor the session actor ".repeat(10)).unwrap();
        assert!(t.chars().count() <= MAX_AUTO_TITLE, "{t}");
        assert!(t.ends_with('…') && !t.ends_with(" …"), "{t}");
        let t = from_prompt(&"x".repeat(200)).unwrap();
        assert_eq!(t.chars().count(), MAX_AUTO_TITLE);
        let t = from_prompt(&"é".repeat(200)).unwrap();
        assert_eq!(t.chars().count(), MAX_AUTO_TITLE);
    }

    #[test]
    fn an_answer_is_tidied() {
        assert_eq!(from_answer("Fix flaky resume test").as_deref(), Some("Fix flaky resume test"));
        assert_eq!(from_answer("\n\"Fix flaky resume test.\"\n").as_deref(), Some("Fix flaky resume test"));
        assert_eq!(from_answer("Title: **dark mode settings**").as_deref(), Some("Dark mode settings"));
        assert_eq!(from_answer("# Session title: Add login\nmore").as_deref(), Some("Add login"));
        assert_eq!(from_answer("   "), None);
        assert_eq!(from_answer("\"\""), None);
        assert_eq!(from_answer(&"word ".repeat(40)), None, "an essay, not a title");
    }

    #[test]
    fn the_ask_quotes_the_request() {
        let a = ask("  fix it  ");
        assert!(a.starts_with(ASK_PREFIX));
        assert!(a.ends_with("<request>\nfix it\n</request>"));
        assert!(ask(&"y".repeat(10_000)).len() < 3000);
    }

    #[test]
    fn the_quick_model_is_picked_by_name() {
        let v = |value: &str, name: &str| SessionConfigValue { value: value.into(), name: name.into(), description: None };
        let mut o = SessionConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: Some("model".into()),
            current_value: "default".into(),
            options: vec![v("default", "Default"), v("opus", "Opus 4.5"), v("haiku", "Haiku 4.5")],
        };
        assert_eq!(quick_model(std::slice::from_ref(&o)), Some(("model".into(), "haiku".into())));
        o.current_value = "haiku".into();
        assert_eq!(quick_model(std::slice::from_ref(&o)), None, "already on it");
        o.options = vec![v("default", "Default"), v("opus", "Opus 4.5")];
        assert_eq!(quick_model(std::slice::from_ref(&o)), None);
        assert_eq!(quick_model(&[]), None);
    }

    #[test]
    fn sources_rank_agent_over_asked_over_prompt() {
        assert!(TitleSource::Agent > TitleSource::Asked && TitleSource::Asked > TitleSource::Prompt);
        for s in [TitleSource::Prompt, TitleSource::Asked, TitleSource::Agent] {
            assert_eq!(TitleSource::parse(s.as_str()), Some(s));
        }
    }
}
