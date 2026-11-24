use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn search(root: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dayone"));
    command
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("DAYONE_CONFIG_DIR", root.join("config/dayone-cli"))
        .env("DAYONE_TELEMETRY", "0")
        .env("DO_NOT_TRACK", "1")
        .env("HF_ENDPOINT", "http://127.0.0.1:0")
        .current_dir(cwd)
        .args(["search", "entries", "--query", "coffee", "--limit", "1"]);
    command
}

fn default_model_cache(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("Library/Caches/dayone-cli/models")
    } else {
        root.join("cache/dayone-cli/models")
    }
}

fn assert_cache_error(output: Output, cache: &Path) {
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(cache.to_str().unwrap()), "{stderr}");
    assert!(stderr.contains("FASTEMBED_CACHE_DIR"), "{stderr}");
    assert_eq!(std::fs::read(cache).unwrap(), b"not a directory");
}

#[test]
fn search_uses_the_user_cache_independently_of_the_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let cache = default_model_cache(root.path());
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"not a directory").unwrap();

    for name in ["project-one", "project-two"] {
        let cwd = root.path().join(name);
        std::fs::create_dir(&cwd).unwrap();
        assert_cache_error(search(root.path(), &cwd).output().unwrap(), &cache);
        assert!(!cwd.join(".fastembed_cache").exists());
    }
}

#[test]
fn search_preserves_cache_overrides_without_fallback() {
    let root = tempfile::tempdir().unwrap();
    let fastembed_cache = root.path().join("explicit cache");
    let hf_cache = root.path().join("huggingface cache");
    std::fs::write(&fastembed_cache, b"not a directory").unwrap();
    std::fs::write(&hf_cache, b"not a directory").unwrap();

    let mut command = search(root.path(), root.path());
    command.env("FASTEMBED_CACHE_DIR", &fastembed_cache);
    assert_cache_error(command.output().unwrap(), &fastembed_cache);

    command.env("HF_HOME", &hf_cache);
    assert_cache_error(command.output().unwrap(), &hf_cache);
    assert!(!default_model_cache(root.path()).exists());
    assert!(!root.path().join(".fastembed_cache").exists());
}
