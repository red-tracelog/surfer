use crate::config::LanguageSetting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiLanguage {
    En,
    De,
}

pub fn resolve_language(setting: LanguageSetting) -> UiLanguage {
    match setting {
        LanguageSetting::En => UiLanguage::En,
        LanguageSetting::De => UiLanguage::De,
        LanguageSetting::Auto => detect_system_language(),
    }
}

fn detect_system_language() -> UiLanguage {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::env;

        let locale = env::var("LC_ALL")
            .ok()
            .or_else(|| env::var("LC_MESSAGES").ok())
            .or_else(|| env::var("LANG").ok())
            .unwrap_or_default()
            .to_lowercase();

        if locale.starts_with("de") {
            UiLanguage::De
        } else {
            UiLanguage::En
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        UiLanguage::En
    }
}

pub fn tr(lang: UiLanguage, key: &str) -> &str {
    match (lang, key) {
        (UiLanguage::En, "menu.file") => "File",
        (UiLanguage::De, "menu.file") => "Datei",

        (UiLanguage::En, "menu.view") => "View",
        (UiLanguage::De, "menu.view") => "Ansicht",

        (UiLanguage::En, "menu.settings") => "Settings",
        (UiLanguage::De, "menu.settings") => "Einstellungen",

        (UiLanguage::En, "menu.help") => "Help",
        (UiLanguage::De, "menu.help") => "Hilfe",

        (UiLanguage::En, "file.open") => "Open file...",
        (UiLanguage::De, "file.open") => "Datei öffnen...",

        (UiLanguage::En, "file.reload") => "Reload",
        (UiLanguage::De, "file.reload") => "Neu laden",

        (UiLanguage::En, "dialog.state_detected.title") => "State file detected",
        (UiLanguage::De, "dialog.state_detected.title") => "Statusdatei erkannt",

        (UiLanguage::En, "dialog.state_detected.body") => {
            "A state file was detected in the same directory as the loaded file.\nLoad state?"
        }
        (UiLanguage::De, "dialog.state_detected.body") => {
            "Im selben Verzeichnis wie die geladene Datei wurde eine Statusdatei erkannt.\nStatus laden?"
        }

        (UiLanguage::En, "dialog.remember_session") => "Remember my decision for this session",
        (UiLanguage::De, "dialog.remember_session") => {
            "Meine Entscheidung für diese Sitzung merken"
        }

        (UiLanguage::En, "dialog.load") => "Load",
        (UiLanguage::De, "dialog.load") => "Laden",

        (UiLanguage::En, "dialog.dont_load") => "Don't load",
        (UiLanguage::De, "dialog.dont_load") => "Nicht laden",

        (UiLanguage::En, "dialog.file_change.title") => "File Change",
        (UiLanguage::De, "dialog.file_change.title") => "Dateiänderung",

        (UiLanguage::En, "dialog.file_change.body") => "File on disk has changed. Reload?",
        (UiLanguage::De, "dialog.file_change.body") => {
            "Die Datei auf dem Datenträger wurde geändert. Neu laden?"
        }

        (UiLanguage::En, "dialog.leave") => "Leave",
        (UiLanguage::De, "dialog.leave") => "Beibehalten",

        _ => match key {
            "menu.file" => "File",
            "menu.view" => "View",
            "menu.settings" => "Settings",
            "menu.help" => "Help",
            "file.open" => "Open file...",
            "file.reload" => "Reload",
            "dialog.state_detected.title" => "State file detected",
            "dialog.state_detected.body" => {
                "A state file was detected in the same directory as the loaded file.\nLoad state?"
            }
            "dialog.remember_session" => "Remember my decision for this session",
            "dialog.load" => "Load",
            "dialog.dont_load" => "Don't load",
            "dialog.file_change.title" => "File Change",
            "dialog.file_change.body" => "File on disk has changed. Reload?",
            "dialog.leave" => "Leave",
            _ => key,
        },
    }
}
