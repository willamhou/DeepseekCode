use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::cli::app::{
    UpdateAction, UpdateArgs, UpdateHomebrewFormulaArgs, UpdateInstallPackageArgs,
    UpdatePackageArgs, UpdateRollbackArgs, UpdateVerifyInstallArgs,
};
use crate::error::{app_error, AppResult};
use crate::util::json::json_escape;

const DEFAULT_RELEASE_DIR: &str = "target/deepseek-release";
const DEFAULT_ROLLBACK_DIR: &str = ".local/bin/deepseek-rollback";

pub fn run(args: UpdateArgs) -> AppResult<()> {
    match &args.action {
        UpdateAction::Status => run_status(args),
        UpdateAction::Package(package_args) => run_package(package_args),
        UpdateAction::VerifyInstall(verify_args) => run_verify_install(verify_args),
        UpdateAction::InstallPackage(install_args) => run_install_package(install_args),
        UpdateAction::Rollback(rollback_args) => run_rollback(rollback_args),
        UpdateAction::HomebrewFormula(formula_args) => run_homebrew_formula(formula_args),
    }
}

fn run_status(args: UpdateArgs) -> AppResult<()> {
    let repo = repo_root();
    let command = update_command(&repo);
    if args.print_command {
        println!("{command}");
        return Ok(());
    }

    println!("DeepSeekCode update");
    println!("  current_version: {}", env!("CARGO_PKG_VERSION"));
    println!("  install_source: source checkout");
    println!("  repo: {}", repo.display());
    println!("  command: {command}");
    println!(
        "  release_package: deepseek update package --bin target/release/{}",
        binary_name()
    );
    println!("  install_verify: deepseek update verify-install --bin <path-to-deepseek>");
    println!("  homebrew_formula: deepseek update homebrew-formula --dist <release-sha-dir>");
    if args.check {
        println!("  check: update command is available");
    } else {
        println!("  next: rerun with --print-command for scripting, or execute the command above");
    }
    Ok(())
}

fn run_package(args: &UpdatePackageArgs) -> AppResult<()> {
    let bin = args
        .bin
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let out_root = args
        .out
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_RELEASE_DIR));
    let package = create_release_package(&bin, &out_root)?;

    println!("DeepSeekCode release package");
    println!("  package: {}", package.package_dir.display());
    println!("  binary: {}", package.binary.display());
    println!("  manifest: {}", package.manifest.display());
    println!("  install_script: {}", package.install_script.display());
    println!("  rollback_script: {}", package.rollback_script.display());
    println!("  services: {}", package.services_doc.display());
    println!(
        "  service_templates: {}",
        package.service_templates.display()
    );
    println!(
        "  verify: deepseek update verify-install --bin {}",
        package.binary.display()
    );
    Ok(())
}

fn run_verify_install(args: &UpdateVerifyInstallArgs) -> AppResult<()> {
    let bin = args
        .bin
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let workdir = args
        .workdir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_verify_workdir);
    let report = verify_install(&bin, &workdir)?;

    println!("DeepSeekCode install verification");
    println!("  binary: {}", report.binary.display());
    println!("  workdir: {}", report.workdir.display());
    for step in &report.steps {
        println!("  {}: ok", step.name);
    }
    println!("  report: {}", report.report.display());
    if args.keep_workdir {
        println!("  cleanup: kept verifier workdir");
    } else {
        std::fs::remove_dir_all(&workdir)?;
        println!("  cleanup: removed verifier workdir");
    }
    Ok(())
}

fn run_install_package(args: &UpdateInstallPackageArgs) -> AppResult<()> {
    let package = args
        .package
        .as_ref()
        .map(PathBuf::from)
        .ok_or_else(|| app_error("update install-package requires --package <dir>"))?;
    let dest = args
        .dest
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_install_path);
    let backup_dir = args
        .backup_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_rollback_dir);
    let result = install_package(&package, &dest, &backup_dir, args.dry_run)?;

    println!("DeepSeekCode package install");
    println!("  package: {}", package.display());
    println!("  source: {}", result.source.display());
    println!("  dest: {}", result.dest.display());
    println!("  backup: {}", result.backup.display());
    println!("  dry_run: {}", args.dry_run);
    Ok(())
}

fn run_rollback(args: &UpdateRollbackArgs) -> AppResult<()> {
    let backup = args
        .backup
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_backup_path);
    let dest = args
        .dest
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_install_path);
    rollback_install(&backup, &dest, args.dry_run)?;

    println!("DeepSeekCode rollback");
    println!("  backup: {}", backup.display());
    println!("  dest: {}", dest.display());
    println!("  dry_run: {}", args.dry_run);
    Ok(())
}

fn run_homebrew_formula(args: &UpdateHomebrewFormulaArgs) -> AppResult<()> {
    let dist = PathBuf::from(&args.dist);
    let formula = render_homebrew_formula(&args.version, &args.repo, &read_homebrew_shas(&dist)?)?;
    let output = args
        .out
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&args.formula));
    std::fs::write(&output, formula)?;
    println!("DeepSeekCode Homebrew formula");
    println!("  version: {}", args.version);
    println!("  repo: {}", args.repo);
    println!("  dist: {}", dist.display());
    println!("  formula: {}", output.display());
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomebrewShas {
    linux_x64: String,
    macos_x64: String,
    macos_arm64: String,
}

fn read_homebrew_shas(dist: &Path) -> AppResult<HomebrewShas> {
    Ok(HomebrewShas {
        linux_x64: read_sha256_file(&dist.join("deepseek-linux-x64.tar.gz.sha256"))?,
        macos_x64: read_sha256_file(&dist.join("deepseek-macos-x64.tar.gz.sha256"))?,
        macos_arm64: read_sha256_file(&dist.join("deepseek-macos-arm64.tar.gz.sha256"))?,
    })
}

fn read_sha256_file(path: &Path) -> AppResult<String> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        app_error(format!(
            "failed to read release checksum {}: {error}",
            path.display()
        ))
    })?;
    let sha = content
        .split_whitespace()
        .next()
        .ok_or_else(|| app_error(format!("empty checksum file: {}", path.display())))?;
    if sha.len() == 64 && sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(sha.to_ascii_lowercase())
    } else {
        Err(app_error(format!(
            "invalid sha256 in {}: expected 64 hex characters",
            path.display()
        )))
    }
}

fn render_homebrew_formula(version: &str, repo: &str, shas: &HomebrewShas) -> AppResult<String> {
    if version.trim().is_empty() {
        return Err(app_error("homebrew formula version must not be empty"));
    }
    if repo.split('/').count() != 2 || repo.contains(' ') {
        return Err(app_error("homebrew formula repo must use owner/name"));
    }
    let tag = format!("v{version}");
    Ok(format!(
        r##"class Deepseek < Formula
  desc "DeepSeek-first terminal code agent"
  homepage "https://github.com/{repo}"
  version "{version}"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/{repo}/releases/download/{tag}/deepseek-macos-arm64.tar.gz"
      sha256 "{macos_arm64}"
    else
      url "https://github.com/{repo}/releases/download/{tag}/deepseek-macos-x64.tar.gz"
      sha256 "{macos_x64}"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/{repo}/releases/download/{tag}/deepseek-linux-x64.tar.gz"
      sha256 "{linux_x64}"
    else
      odie "DeepSeekCode Homebrew formula currently publishes Linux x64 only"
    end
  end

  def install
    binary = Dir["deepseek*/deepseek"].first || "deepseek"
    bin.install binary => "deepseek"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/deepseek version")
    system "#{{bin}}/deepseek", "doctor", "--json"
  end
end
"##,
        repo = repo,
        version = version,
        tag = tag,
        macos_arm64 = shas.macos_arm64,
        macos_x64 = shas.macos_x64,
        linux_x64 = shas.linux_x64,
    ))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn update_command(repo: &Path) -> String {
    format!(
        "cargo install --path {} --bin deepseek --force",
        shell_quote(&repo.display().to_string())
    )
}

#[derive(Debug, Clone)]
struct ReleasePackage {
    package_dir: PathBuf,
    binary: PathBuf,
    manifest: PathBuf,
    install_script: PathBuf,
    rollback_script: PathBuf,
    services_doc: PathBuf,
    service_templates: PathBuf,
}

fn create_release_package(bin: &Path, out_root: &Path) -> AppResult<ReleasePackage> {
    if !bin.is_file() {
        return Err(app_error(format!(
            "release binary not found: {}",
            bin.display()
        )));
    }

    let package_dir = out_root.join(package_dir_name());
    std::fs::create_dir_all(&package_dir)?;
    let binary = package_dir.join(binary_name());
    std::fs::copy(bin, &binary)?;
    preserve_executable(bin, &binary)?;

    let manifest = package_dir.join("release.json");
    std::fs::write(&manifest, release_manifest(bin, &binary)?)?;
    let install_script_path = package_dir.join("install.sh");
    std::fs::write(&install_script_path, install_script())?;
    set_executable(&install_script_path)?;
    let rollback_script_path = package_dir.join("rollback.sh");
    std::fs::write(&rollback_script_path, rollback_script())?;
    set_executable(&rollback_script_path)?;
    std::fs::write(package_dir.join("VERIFY.md"), verify_instructions(&binary))?;
    let services_doc = package_dir.join("SERVICES.md");
    std::fs::write(&services_doc, service_instructions())?;
    let service_templates = package_dir.join("services");
    write_packaged_service_templates(&service_templates)?;

    Ok(ReleasePackage {
        package_dir,
        binary,
        manifest,
        install_script: install_script_path,
        rollback_script: rollback_script_path,
        services_doc,
        service_templates,
    })
}

fn release_manifest(source: &Path, packaged_binary: &Path) -> AppResult<String> {
    let metadata = std::fs::metadata(packaged_binary)?;
    let commit = git_commit().unwrap_or_else(|| "unknown".to_string());
    Ok(format!(
        "{{\n  \"name\": \"deepseek\",\n  \"version\": \"{}\",\n  \"platform\": \"{}\",\n  \"commit\": \"{}\",\n  \"source_binary\": \"{}\",\n  \"packaged_binary\": \"{}\",\n  \"size_bytes\": {}\n}}\n",
        env!("CARGO_PKG_VERSION"),
        platform_label(),
        json_escape(&commit),
        json_escape(&source.display().to_string()),
        json_escape(&packaged_binary.display().to_string()),
        metadata.len()
    ))
}

fn install_script() -> &'static str {
    r#"#!/bin/sh
set -eu
DEST="${1:-$HOME/.local/bin/deepseek}"
BACKUP_DIR="${DSCODE_ROLLBACK_DIR:-$HOME/.local/bin/deepseek-rollback}"
mkdir -p "$(dirname "$DEST")" "$BACKUP_DIR"
if [ -f "$DEST" ]; then
  cp "$DEST" "$BACKUP_DIR/deepseek.previous"
fi
cp "$(dirname "$0")/deepseek" "$DEST"
chmod +x "$DEST"
"#
}

fn rollback_script() -> &'static str {
    r#"#!/bin/sh
set -eu
DEST="${1:-$HOME/.local/bin/deepseek}"
BACKUP="${2:-$HOME/.local/bin/deepseek-rollback/deepseek.previous}"
cp "$BACKUP" "$DEST"
chmod +x "$DEST"
"#
}

fn verify_instructions(binary: &Path) -> String {
    format!(
        "# Verify DeepSeekCode Release\n\nRun:\n\n```bash\n{} update verify-install --bin {}\n```\n\nThe verifier runs `version`, `config init --force`, `doctor`, `exec --json`, and a one-case benchmark in an isolated directory.\n",
        binary.display(),
        binary.display()
    )
}

fn service_instructions() -> &'static str {
    r#"# Runtime Service Templates

After installing the package binary, render supervisor files for the target
workspace:

```bash
deepseek agents service --kind systemd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
deepseek agents service --kind launchd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
```

The generated set runs the HTTP runtime (`deepseek serve --http`), the durable
task daemon (`deepseek agents daemon --json`), and the diagnostics watch worker
(`deepseek diagnostics --watch --changed`). Review the generated
WorkingDirectory, bind address, poll interval, and budget before installing the
files with systemd or launchd.
"#
}

fn write_packaged_service_templates(root: &Path) -> AppResult<()> {
    let templates = [
        (
            "systemd/deepseek-runtime.service",
            include_str!("../../../packaging/systemd/deepseek-runtime.service"),
        ),
        (
            "systemd/deepseek-agents.service",
            include_str!("../../../packaging/systemd/deepseek-agents.service"),
        ),
        (
            "systemd/deepseek-diagnostics.service",
            include_str!("../../../packaging/systemd/deepseek-diagnostics.service"),
        ),
        (
            "launchd/com.deepseek.runtime.plist",
            include_str!("../../../packaging/launchd/com.deepseek.runtime.plist"),
        ),
        (
            "launchd/com.deepseek.agents.plist",
            include_str!("../../../packaging/launchd/com.deepseek.agents.plist"),
        ),
        (
            "launchd/com.deepseek.diagnostics.plist",
            include_str!("../../../packaging/launchd/com.deepseek.diagnostics.plist"),
        ),
    ];
    for (relative, body) in templates {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, body)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct InstallVerifyReport {
    binary: PathBuf,
    workdir: PathBuf,
    report: PathBuf,
    steps: Vec<VerifyStep>,
}

#[derive(Debug, Clone)]
struct VerifyStep {
    name: &'static str,
}

fn verify_install(bin: &Path, workdir: &Path) -> AppResult<InstallVerifyReport> {
    if !bin.is_file() {
        return Err(app_error(format!(
            "install verifier binary not found: {}",
            bin.display()
        )));
    }
    let bin = std::fs::canonicalize(bin)?;
    std::fs::create_dir_all(workdir)?;
    let manifest = workdir.join("install-verify-benchmark.txt");
    let report = workdir.join("install-verify-report.md");
    std::fs::write(&manifest, install_verify_manifest())?;

    let mut steps = Vec::new();
    run_step("version", &bin, &["version"], workdir)?;
    steps.push(VerifyStep { name: "version" });
    run_step("config_init", &bin, &["config", "init", "--force"], workdir)?;
    steps.push(VerifyStep {
        name: "config_init",
    });
    run_step("doctor", &bin, &["doctor"], workdir)?;
    steps.push(VerifyStep { name: "doctor" });
    run_step(
        "exec_json",
        &bin,
        &["exec", "--json", "--budget", "1", "say install verifier ok"],
        workdir,
    )?;
    steps.push(VerifyStep { name: "exec_json" });

    let manifest_arg = manifest.display().to_string();
    let report_arg = report.display().to_string();
    run_step(
        "benchmark_sample",
        &bin,
        &[
            "benchmark",
            "--manifest",
            &manifest_arg,
            "--out",
            &report_arg,
        ],
        workdir,
    )?;
    steps.push(VerifyStep {
        name: "benchmark_sample",
    });

    Ok(InstallVerifyReport {
        binary: bin,
        workdir: workdir.to_path_buf(),
        report,
        steps,
    })
}

fn install_verify_manifest() -> &'static str {
    r#"name = "install-verify-sample"
task = "say the install verifier benchmark is working"
category = "read_only"
budget = 1
max_failed_tools = 0
"#
}

fn run_step(name: &str, bin: &Path, args: &[&str], cwd: &Path) -> AppResult<Output> {
    let output = run_step_command(bin, args, cwd).map_err(|error| {
        app_error(format!(
            "could not run install verifier step {name}: {error}"
        ))
    })?;
    if !output.status.success() {
        return Err(app_error(format!(
            "install verifier step `{name}` failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

fn run_step_command(bin: &Path, args: &[&str], cwd: &Path) -> std::io::Result<Output> {
    const ETXTBSY: i32 = 26;
    let mut attempts = 0;
    loop {
        let result = Command::new(bin)
            .args(args)
            .current_dir(cwd)
            .env("DSCODE_AUTO_APPROVE_WRITES", "1")
            .env("DSCODE_AUTO_APPROVE_SHELL", "1")
            .env("DSCODE_AUTO_APPROVE_MCP", "1")
            .output();
        match result {
            Err(error) if error.raw_os_error() == Some(ETXTBSY) && attempts < 5 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

#[derive(Debug, Clone)]
struct InstallPackageResult {
    source: PathBuf,
    dest: PathBuf,
    backup: PathBuf,
}

fn install_package(
    package: &Path,
    dest: &Path,
    backup_dir: &Path,
    dry_run: bool,
) -> AppResult<InstallPackageResult> {
    let source = package.join(binary_name());
    if !source.is_file() {
        return Err(app_error(format!(
            "package binary not found: {}",
            source.display()
        )));
    }
    let backup = backup_dir.join("deepseek.previous");
    if !dry_run {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(backup_dir)?;
        if dest.is_file() {
            std::fs::copy(dest, &backup)?;
        }
        std::fs::copy(&source, dest)?;
        preserve_executable(&source, dest)?;
    }
    Ok(InstallPackageResult {
        source,
        dest: dest.to_path_buf(),
        backup,
    })
}

fn rollback_install(backup: &Path, dest: &Path, dry_run: bool) -> AppResult<()> {
    if !backup.is_file() {
        return Err(app_error(format!(
            "rollback backup not found: {}",
            backup.display()
        )));
    }
    if !dry_run {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(backup, dest)?;
        preserve_executable(backup, dest)?;
    }
    Ok(())
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "deepseek.exe"
    } else {
        "deepseek"
    }
}

fn package_dir_name() -> String {
    format!(
        "deepseek-{}-{}",
        env!("CARGO_PKG_VERSION"),
        platform_label()
    )
}

fn platform_label() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn default_install_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/bin")
        .join(binary_name())
}

fn default_rollback_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_ROLLBACK_DIR)
}

fn default_backup_path() -> PathBuf {
    default_rollback_dir().join("deepseek.previous")
}

fn default_verify_workdir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "deepseek-install-verify-{}-{nanos}",
        std::process::id()
    ))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn git_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

#[cfg(unix)]
fn preserve_executable(source: &Path, dest: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(source)?.permissions().mode();
    let mut permissions = std::fs::metadata(dest)?.permissions();
    permissions.set_mode(mode | 0o755);
    std::fs::set_permissions(dest, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn preserve_executable(_source: &Path, _dest: &Path) -> AppResult<()> {
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> AppResult<()> {
    Ok(())
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_command_uses_cargo_install_path() {
        let command = update_command(Path::new("/tmp/deepseek-code"));
        assert_eq!(
            command,
            "cargo install --path /tmp/deepseek-code --bin deepseek --force"
        );
    }

    #[test]
    fn shell_quote_quotes_spaces() {
        assert_eq!(shell_quote("/tmp/Deepseek Code"), "'/tmp/Deepseek Code'");
    }

    #[test]
    fn install_verify_manifest_is_minimal_benchmark_case() {
        let manifest = install_verify_manifest();
        assert!(manifest.contains("install-verify-sample"));
        assert!(manifest.contains("max_failed_tools = 0"));
    }

    #[test]
    fn package_dir_name_includes_version_and_platform() {
        let name = package_dir_name();
        assert!(name.starts_with("deepseek-"));
        assert!(name.contains(std::env::consts::OS));
    }

    #[test]
    fn render_homebrew_formula_uses_release_shas() {
        let formula = render_homebrew_formula(
            "1.2.3",
            "example/deepseek",
            &HomebrewShas {
                linux_x64: "a".repeat(64),
                macos_x64: "b".repeat(64),
                macos_arm64: "c".repeat(64),
            },
        )
        .unwrap();

        assert!(formula.contains("version \"1.2.3\""));
        assert!(formula.contains(
            "https://github.com/example/deepseek/releases/download/v1.2.3/deepseek-macos-arm64.tar.gz"
        ));
        assert!(formula.contains(&format!("sha256 \"{}\"", "a".repeat(64))));
        assert!(formula.contains(&format!("sha256 \"{}\"", "b".repeat(64))));
        assert!(formula.contains(&format!("sha256 \"{}\"", "c".repeat(64))));
    }

    #[test]
    fn read_homebrew_shas_reads_release_matrix_files() {
        let root = temp_root("homebrew-shas");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("deepseek-linux-x64.tar.gz.sha256"),
            format!("{}  deepseek-linux-x64.tar.gz\n", "a".repeat(64)),
        )
        .unwrap();
        std::fs::write(
            root.join("deepseek-macos-x64.tar.gz.sha256"),
            format!("{}  deepseek-macos-x64.tar.gz\n", "b".repeat(64)),
        )
        .unwrap();
        std::fs::write(
            root.join("deepseek-macos-arm64.tar.gz.sha256"),
            format!("{}  deepseek-macos-arm64.tar.gz\n", "C".repeat(64)),
        )
        .unwrap();

        let shas = read_homebrew_shas(&root).unwrap();

        assert_eq!(shas.linux_x64, "a".repeat(64));
        assert_eq!(shas.macos_x64, "b".repeat(64));
        assert_eq!(shas.macos_arm64, "c".repeat(64));
    }

    #[test]
    fn read_sha256_file_rejects_invalid_checksum() {
        let root = temp_root("bad-sha");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bad.sha256");
        std::fs::write(&path, "not-a-sha  bad.tar.gz\n").unwrap();

        let error = read_sha256_file(&path).unwrap_err();

        assert!(error.to_string().contains("invalid sha256"));
    }

    #[test]
    #[cfg(unix)]
    fn create_release_package_copies_binary_and_writes_scripts() {
        let root = temp_root("package");
        let bin = root.join("bin/deepseek");
        write_executable(&bin, "#!/bin/sh\necho deepseek fake\n");
        let out = root.join("out");

        let package = create_release_package(&bin, &out).unwrap();

        assert!(package.binary.is_file());
        assert!(package.manifest.is_file());
        assert!(package.install_script.is_file());
        assert!(package.rollback_script.is_file());
        assert!(package.services_doc.is_file());
        assert!(package
            .service_templates
            .join("systemd/deepseek-runtime.service")
            .is_file());
        assert!(package
            .service_templates
            .join("systemd/deepseek-diagnostics.service")
            .is_file());
        assert!(package
            .service_templates
            .join("launchd/com.deepseek.agents.plist")
            .is_file());
        assert!(package
            .service_templates
            .join("launchd/com.deepseek.diagnostics.plist")
            .is_file());
        assert!(package.package_dir.join("VERIFY.md").is_file());
        let manifest = std::fs::read_to_string(package.manifest).unwrap();
        assert!(manifest.contains("\"name\": \"deepseek\""));
        assert!(manifest.contains("\"version\":"));
        let services = std::fs::read_to_string(package.services_doc).unwrap();
        assert!(services.contains("deepseek agents service"));
    }

    #[test]
    #[cfg(unix)]
    fn verify_install_runs_expected_steps_with_fake_binary() {
        let root = temp_root("verify");
        let bin = root.join("deepseek");
        write_fake_deepseek(&bin);
        let workdir = root.join("work");

        let report = verify_install(&bin, &workdir).unwrap();

        let names = report
            .steps
            .iter()
            .map(|step| step.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "version",
                "config_init",
                "doctor",
                "exec_json",
                "benchmark_sample"
            ]
        );
        assert!(workdir.join("install-verify-benchmark.txt").is_file());
    }

    #[test]
    #[cfg(unix)]
    fn install_package_backs_up_existing_dest_and_rollback_restores_it() {
        let root = temp_root("install");
        let package = root.join("package");
        std::fs::create_dir_all(&package).unwrap();
        let source = package.join("deepseek");
        write_executable(&source, "#!/bin/sh\necho new\n");
        let dest = root.join("bin/deepseek");
        write_executable(&dest, "#!/bin/sh\necho old\n");
        let backup_dir = root.join("rollback");

        let result = install_package(&package, &dest, &backup_dir, false).unwrap();

        assert!(result.backup.is_file());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho new\n"
        );

        rollback_install(&result.backup, &dest, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho old\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_package_dry_run_does_not_replace_dest() {
        let root = temp_root("dry-run");
        let package = root.join("package");
        std::fs::create_dir_all(&package).unwrap();
        write_executable(&package.join("deepseek"), "#!/bin/sh\necho new\n");
        let dest = root.join("bin/deepseek");
        write_executable(&dest, "#!/bin/sh\necho old\n");

        install_package(&package, &dest, &root.join("rollback"), true).unwrap();

        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho old\n"
        );
    }

    #[cfg(unix)]
    fn write_fake_deepseek(path: &Path) {
        write_executable(
            path,
            r##"#!/bin/sh
set -eu
case "${1:-}" in
  version)
    echo "deepseek 0.0.0-test"
    ;;
  config)
    mkdir -p .dscode
    echo "config"
    ;;
  doctor)
    echo "doctor ok"
    ;;
  exec)
    echo '{"event":"session_started"}'
    echo '{"event":"assistant_final","message":"ok"}'
    ;;
  benchmark)
    out=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--out" ]; then
        shift
        out="$1"
      fi
      shift || true
    done
    if [ -n "$out" ]; then
      mkdir -p "$(dirname "$out")"
      echo "# fake report" > "$out"
    fi
    echo "benchmark ok"
    ;;
  *)
    echo "unexpected command: ${1:-}" >&2
    exit 2
    ;;
esac
"##,
        );
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn temp_root(name: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-update-{name}-{}-{suffix}",
            std::process::id()
        ))
    }
}
