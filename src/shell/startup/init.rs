//! Loads `init.lua` from the config directory (U2).

use crate::core::editor::Editor;
use crate::shell::{lua, run_source};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

/// Runs `init.lua` from the config directory, if there is one (U2).
fn load_init_from(ed: Editor, lua: &mlua::Lua, xdg: Option<&OsStr>, home: Option<&Path>) -> Editor {
    let Some(path) = lua::config_path(xdg, home) else {
        return ed;
    };
    let Ok(source) = fs::read_to_string(&path) else {
        return ed;
    };
    let (ed, result) = run_source(ed, lua, &source);
    if let Err(e) = result {
        let msg = format!(
            "init.lua: {}",
            e.to_string().lines().next().unwrap_or_default()
        );
        return ed.set_message(msg);
    }
    ed
}

pub fn load_init(ed: Editor, lua: &mlua::Lua) -> Editor {
    let xdg = env::var_os("XDG_CONFIG_HOME");
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    load_init_from(ed, lua, xdg.as_deref(), home.as_deref().map(Path::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;

    #[test]
    fn load_init_missing_and_adversarial_lua_handling() {
        let lua = lua::new_state().unwrap();
        let ed = Editor::new(Config::default());

        // 1. Missing / None paths
        let ed = load_init_from(ed, &lua, None, None);
        assert_eq!(ed.message(), "");

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("neonemacs");
        fs::create_dir_all(&config_dir).unwrap();
        let init_file = config_dir.join("init.lua");

        // 2. Valid Lua script modifying config
        fs::write(&init_file, r#"set("fill-column", 85)"#).unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert_eq!(ed.config().fill_column, 85);
        assert_eq!(ed.message(), "");

        // 3. Syntax error in init.lua
        fs::write(&init_file, "function bad_syntax(").unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert!(ed.message().starts_with("init.lua: "));

        // 4. Runtime error in init.lua
        fs::write(&init_file, r#"error("adversarial error")"#).unwrap();
        let ed = load_init_from(ed, &lua, Some(dir.path().as_os_str()), None);
        assert!(ed.message().starts_with("init.lua: "));
        assert!(ed.message().contains("adversarial error"));
    }
}
