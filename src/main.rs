use std::path::{Path, PathBuf};

use hive_router::{
    configure_global_allocator, error::RouterInitError, init_rustls_crypto_provider, ntex,
    router_entrypoint, PluginRegistry, RouterGlobalAllocator,
};

mod config_template;

configure_global_allocator!();

/// Config file names Hive Router looks for in the working directory when
/// `ROUTER_CONFIG_FILE_PATH` is unset. Mirrors `hive_router_config::DEFAULT_FILE_NAMES`,
/// which is private — keep in sync when bumping hive-router.
const DEFAULT_CONFIG_FILE_NAMES: &[&str] = &[
    "router.config.yaml",
    "router.config.yml",
    "router.config.json",
    "router.config.json5",
];

#[hive_router::main]
async fn main() -> Result<(), RouterInitError> {
    init_rustls_crypto_provider();

    // Render `${VAR}` in the config from the environment before hive-router
    // reads it. Fail hard rather than booting with a half-substituted config:
    // a missing secret would otherwise surface as tokens that silently fail to
    // verify. This runs before telemetry/logging init, so it reports on stderr.
    if let Err(error) = prepare_config() {
        eprintln!("evolve-gateway: failed to prepare router config: {error}");
        std::process::exit(1);
    }

    router_entrypoint(
        PluginRegistry::new()
            .register::<hive_router_federated_token::plugin::FederatedTokenPlugin>(),
    )
    .await
}

/// Render the router config template and point `ROUTER_CONFIG_FILE_PATH` at the
/// result. No-op when there is no config file at all — hive-router then runs on
/// its own defaults.
fn prepare_config() -> Result<(), Box<dyn std::error::Error>> {
    let Some(source) = resolve_config_path() else {
        return Ok(());
    };

    let template = std::fs::read_to_string(&source)
        .map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    let rendered =
        config_template::render(&template).map_err(|e| format!("{}: {e}", source.display()))?;

    let sibling = rendered_path(&source);
    let target = match write_private(&sibling, &rendered) {
        Ok(()) => sibling,
        // A mounted ConfigMap/secret volume is read-only, so falling back to a
        // writable directory is the difference between running and not. Relative
        // paths inside the config resolve against the config's own directory, so
        // warn — projects in this situation should use HIVE_CDN_ENDPOINT or an
        // absolute SUPERGRAPH_FILE_PATH.
        Err(sibling_error) => {
            let fallback = std::env::temp_dir().join(
                sibling
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new(".router.config.rendered.yaml")),
            );
            write_private(&fallback, &rendered).map_err(|e| {
                format!(
                    "cannot write rendered config to {} ({sibling_error}) or {} ({e})",
                    sibling.display(),
                    fallback.display()
                )
            })?;
            eprintln!(
                "evolve-gateway: {} is not writable, rendered config written to {}. \
                 Relative paths in the config now resolve against that directory.",
                sibling.parent().unwrap_or(Path::new(".")).display(),
                fallback.display()
            );
            fallback
        }
    };

    std::env::set_var("ROUTER_CONFIG_FILE_PATH", &target);

    Ok(())
}

/// The config file hive-router would have loaded: an explicit
/// `ROUTER_CONFIG_FILE_PATH`, else the first default name present in the
/// working directory.
fn resolve_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ROUTER_CONFIG_FILE_PATH") {
        // Let hive-router produce the "required file is missing" error itself,
        // so the message matches what an operator would get without this crate.
        return Some(PathBuf::from(path));
    }

    DEFAULT_CONFIG_FILE_NAMES
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// Where to put the rendered config.
///
/// It must live in the same directory as the source: hive-router resolves
/// relative paths in the config (notably `supergraph.path`) against the config
/// file's parent, so rendering elsewhere would silently change their meaning.
fn rendered_path(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "router.config.yaml".to_string());

    // Insert `.rendered` before the extension so the `config` crate still
    // detects the format, and hide the file with a leading dot.
    let rendered_name = match name.rsplit_once('.') {
        Some((stem, ext)) => format!(".{stem}.rendered.{ext}"),
        None => format!(".{name}.rendered"),
    };

    source.with_file_name(rendered_name)
}

/// Write the rendered config readable only by the current user — it contains
/// the substituted secrets.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::rendered_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn rendered_config_is_a_sibling_of_its_source() {
        assert_eq!(
            rendered_path(Path::new("/app/router.config.yaml")),
            PathBuf::from("/app/.router.config.rendered.yaml")
        );
    }

    #[test]
    fn rendered_config_keeps_the_source_extension() {
        assert_eq!(
            rendered_path(Path::new("conf/router.config.json")),
            PathBuf::from("conf/.router.config.rendered.json")
        );
    }

    #[test]
    fn rendered_config_handles_an_extensionless_source() {
        assert_eq!(
            rendered_path(Path::new("routerconfig")),
            PathBuf::from(".routerconfig.rendered")
        );
    }
}
