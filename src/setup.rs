//! First-session recommendations use the ordinary store/release/install flow.
use serde::Deserialize;
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Application {
    pub bundle_id: String,
    pub name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    format: u32,
    applications: Vec<Application>,
}
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Application>, String> {
    if bytes.len() > 64 * 1024 {
        return Err("Initial app plan is too large".into());
    }
    let plan: Plan = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if plan.format != 1 || plan.applications.len() > 32 {
        return Err("Unsupported initial app plan".into());
    }
    let mut seen = std::collections::HashSet::new();
    for app in &plan.applications {
        if app.name.trim().is_empty()
            || app.name.len() > 128
            || app.name.chars().any(char::is_control)
            || app.bundle_id.len() > 192
            || !app.bundle_id.contains('.')
            || app.bundle_id.split('.').any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            || !seen.insert(&app.bundle_id)
        {
            return Err("Invalid or repeated initial application".into());
        }
    }
    Ok(plan.applications)
}
pub(crate) fn load() -> Vec<Application> {
    #[cfg(not(target_os = "mochios"))]
    if std::env::var_os("MOCHIOS_PREVIEW_FIRST_RUN").is_none() {
        return vec![];
    }
    match parse(include_bytes!("../initial-apps.json")) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("Initial application setup unavailable: {error}");
            vec![]
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipped_plan_is_data_and_has_unique_application_ids() {
        let plan = parse(include_bytes!("../initial-apps.json")).unwrap();
        assert!(!plan.is_empty());
    }
    #[test]
    fn invalid_plans_are_rejected() {
        for text in [
            r#"{}"#,
            r#"{"format":2,"applications":[]}"#,
            r#"{"format":1,"applications":[{"bundle_id":"../bad","name":"Bad"}]}"#,
            r#"{"format":1,"applications":[{"bundle_id":"org.example.app","name":"One"},{"bundle_id":"org.example.app","name":"Two"}]}"#,
        ] {
            assert!(parse(text.as_bytes()).is_err());
        }
    }
}
