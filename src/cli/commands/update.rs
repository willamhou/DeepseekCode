use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::cli::app::{
    UpdateAction, UpdateArgs, UpdateHomebrewFormulaArgs, UpdateInstallPackageArgs,
    UpdatePackageArgs, UpdatePublishStatusArgs, UpdateRollbackArgs, UpdateVerifyInstallArgs,
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
        UpdateAction::PublishStatus(status_args) => run_publish_status(status_args),
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
    println!("  publish_status: deepseek update publish-status --dist <release-asset-dir> --npm-dist <npm-artifact-dir>");
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

fn run_publish_status(args: &UpdatePublishStatusArgs) -> AppResult<()> {
    let report = build_publish_status_report(&repo_root(), args, &env_value)?;

    if args.json {
        println!("{}", render_publish_status_json(&report, args.strict));
    } else {
        println!("DeepSeekCode publish status");
        println!("  version: {}", report.version);
        println!("  repository: {}", report.repository);
        for check in &report.checks {
            println!(
                "  {}: {} ({})",
                check.name,
                check.status.label(),
                check.detail
            );
        }
        println!("  public_install:");
        for check in &report.public_install {
            println!(
                "    {}: {} ({})",
                check.name,
                check.status.label(),
                check.detail
            );
            println!("      verify: {}", check.verify);
        }
        println!("  not_ready: {}", report.not_ready_count());
        if report.not_ready_count() == 0 {
            println!(
                "  next: tag release can publish npm, Homebrew, GHCR, and GitHub Release assets when workflow gates pass"
            );
        } else {
            println!("  next: configure missing secrets/assets, then rerun with --strict");
        }
    }

    if args.strict && report.not_ready_count() > 0 {
        return Err(app_error(format!(
            "publish status is not ready: {} check(s) are blocked or skipped",
            report.not_ready_count()
        )));
    }
    Ok(())
}

fn render_publish_status_json(report: &PublishStatusReport, strict: bool) -> String {
    let checks = report
        .checks
        .iter()
        .map(|check| {
            format!(
                "{{\"name\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\"}}",
                json_escape(check.name),
                check.status.label(),
                json_escape(&check.detail)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let public_install = report
        .public_install
        .iter()
        .map(|check| {
            format!(
                "{{\"name\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\",\"verify\":\"{}\"}}",
                json_escape(check.name),
                check.status.label(),
                json_escape(&check.detail),
                json_escape(&check.verify)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"kind\":\"deepseek.publish_status.v1\",\"version\":\"{}\",\"repository\":\"{}\",\"strict\":{},\"not_ready\":{},\"checks\":[{}],\"public_install\":[{}]}}",
        json_escape(&report.version),
        json_escape(&report.repository),
        strict,
        report.not_ready_count(),
        checks,
        public_install
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishStatusReport {
    version: String,
    repository: String,
    checks: Vec<PublishStatusCheck>,
    public_install: Vec<PublicInstallCheck>,
}

impl PublishStatusReport {
    fn not_ready_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| !check.status.is_ready())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishStatusCheck {
    name: &'static str,
    status: PublishStatus,
    detail: String,
}

impl PublishStatusCheck {
    fn ready(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: PublishStatus::Ready,
            detail: detail.into(),
        }
    }

    fn blocked(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: PublishStatus::Blocked,
            detail: detail.into(),
        }
    }

    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: PublishStatus::Skipped,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishStatus {
    Ready,
    Blocked,
    Skipped,
}

impl PublishStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Skipped => "skipped",
        }
    }

    fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicInstallCheck {
    name: &'static str,
    status: PublicInstallStatus,
    detail: String,
    verify: String,
}

impl PublicInstallCheck {
    fn new(
        name: &'static str,
        status: PublicInstallStatus,
        detail: impl Into<String>,
        verify: impl Into<String>,
    ) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            verify: verify.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicInstallStatus {
    SourceAvailable,
    ReadyToPublish,
    RequiresPublish,
    SourceOnlyPolicy,
}

impl PublicInstallStatus {
    fn label(self) -> &'static str {
        match self {
            Self::SourceAvailable => "source_available",
            Self::ReadyToPublish => "ready_to_publish",
            Self::RequiresPublish => "requires_publish",
            Self::SourceOnlyPolicy => "source_only_policy",
        }
    }
}

fn build_publish_status_report(
    repo: &Path,
    args: &UpdatePublishStatusArgs,
    env: &impl Fn(&str) -> Option<String>,
) -> AppResult<PublishStatusReport> {
    let cargo_toml = std::fs::read_to_string(repo.join("Cargo.toml"))?;
    let version = toml_string_value(&cargo_toml, "version")
        .ok_or_else(|| app_error("Cargo.toml package version was not found"))?;
    let repository = repository_slug(&cargo_toml, env).unwrap_or_else(|| "owner/repo".to_string());

    let mut checks = Vec::new();
    checks.push(cargo_registry_status(&cargo_toml, env));
    checks.push(npm_metadata_status(repo, &version));
    checks.push(npm_token_status(env));
    checks.push(npm_artifacts_status(args.npm_dist.as_deref(), &version));
    checks.push(release_asset_status(args.dist.as_deref()));
    checks.push(homebrew_template_status(repo, &version));
    checks.push(homebrew_tap_status(env));
    let public_install =
        build_public_install_checks(&repository, &version, &cargo_toml, &checks, env);

    Ok(PublishStatusReport {
        version,
        repository,
        checks,
        public_install,
    })
}

fn build_public_install_checks(
    repository: &str,
    version: &str,
    cargo_toml: &str,
    checks: &[PublishStatusCheck],
    env: &impl Fn(&str) -> Option<String>,
) -> Vec<PublicInstallCheck> {
    let release_ready = publish_check_ready(checks, "release_assets");
    let npm_ready = publish_check_ready(checks, "npm_metadata")
        && publish_check_ready(checks, "npm_token")
        && publish_check_ready(checks, "npm_artifacts");
    let homebrew_ready = publish_check_ready(checks, "homebrew_formula")
        && publish_check_ready(checks, "homebrew_tap")
        && release_ready;
    let cargo_source_only = toml_bool_value(cargo_toml, "publish") == Some(false);
    let tag = format!("v{version}");
    let repository_url = format!("https://github.com/{repository}");
    let ghcr_image = format!("ghcr.io/{}:{version}", repository.to_ascii_lowercase());
    let homebrew_tap =
        env("HOMEBREW_TAP_REPOSITORY").unwrap_or_else(|| "<tap-owner/tap-repo>".to_string());

    vec![
        PublicInstallCheck::new(
            "source_checkout",
            PublicInstallStatus::SourceAvailable,
            "repository metadata provides the source install path; verify GitHub visibility before announcing it",
            format!("git ls-remote {repository_url}.git HEAD"),
        ),
        PublicInstallCheck::new(
            "github_release",
            if release_ready {
                PublicInstallStatus::ReadyToPublish
            } else {
                PublicInstallStatus::RequiresPublish
            },
            if release_ready {
                "local release archives and checksums are present; tag workflow still needs live release verification"
            } else {
                "public binary release evidence is not verified; pass --dist after the release matrix assets are available"
            },
            format!("gh release view {tag} --repo {repository}"),
        ),
        PublicInstallCheck::new(
            "npm",
            if npm_ready {
                PublicInstallStatus::ReadyToPublish
            } else {
                PublicInstallStatus::RequiresPublish
            },
            if npm_ready {
                "npm metadata, registry token, and platform tarballs are ready; publish before advertising npm install"
            } else {
                "npm registry availability is not verified; configure NPM_TOKEN/NODE_AUTH_TOKEN and pass --npm-dist"
            },
            "npm view @deepseek-code/cli version",
        ),
        PublicInstallCheck::new(
            "homebrew",
            if homebrew_ready {
                PublicInstallStatus::ReadyToPublish
            } else {
                PublicInstallStatus::RequiresPublish
            },
            if homebrew_ready {
                "tap configuration, formula template, and release checksums are ready; publish the tap before advertising brew install"
            } else {
                "Homebrew tap availability is not verified; configure HOMEBREW_TAP_REPOSITORY/HOMEBREW_TAP_TOKEN and pass --dist"
            },
            format!("brew tap {homebrew_tap} && brew install deepseek"),
        ),
        PublicInstallCheck::new(
            "ghcr",
            if release_ready {
                PublicInstallStatus::ReadyToPublish
            } else {
                PublicInstallStatus::RequiresPublish
            },
            if release_ready {
                "release assets are present locally; tag workflow must still publish and verify the GHCR image"
            } else {
                "GHCR image availability is not verified; publish from a tag workflow before advertising docker pull"
            },
            format!("docker pull {ghcr_image}"),
        ),
        PublicInstallCheck::new(
            "cargo_registry",
            if cargo_source_only {
                PublicInstallStatus::SourceOnlyPolicy
            } else if publish_check_ready(checks, "cargo_registry") {
                PublicInstallStatus::ReadyToPublish
            } else {
                PublicInstallStatus::RequiresPublish
            },
            if cargo_source_only {
                "Cargo.toml publish=false; Cargo registry distribution is intentionally source-build/package-only"
            } else {
                "Cargo registry publish policy requires token and ownership verification before advertising cargo install"
            },
            if cargo_source_only {
                format!("cargo install --git {repository_url}.git --locked")
            } else {
                "cargo search deepseek-code".to_string()
            },
        ),
    ]
}

fn publish_check_ready(checks: &[PublishStatusCheck], name: &str) -> bool {
    checks
        .iter()
        .any(|check| check.name == name && check.status.is_ready())
}

fn cargo_registry_status(
    cargo_toml: &str,
    env: &impl Fn(&str) -> Option<String>,
) -> PublishStatusCheck {
    if toml_bool_value(cargo_toml, "publish") == Some(false) {
        return PublishStatusCheck::ready(
            "cargo_registry",
            "Cargo.toml publish=false; registry distribution is source-build/package-only by policy",
        );
    }
    if env("CARGO_REGISTRY_TOKEN").is_some() {
        PublishStatusCheck::ready("cargo_registry", "CARGO_REGISTRY_TOKEN is configured")
    } else {
        PublishStatusCheck::skipped(
            "cargo_registry",
            "CARGO_REGISTRY_TOKEN is missing; Cargo publish job will skip",
        )
    }
}

fn npm_metadata_status(repo: &Path, version: &str) -> PublishStatusCheck {
    match npm_metadata_failures(repo, version) {
        Ok(failures) if failures.is_empty() => PublishStatusCheck::ready(
            "npm_metadata",
            "root and platform package versions/licenses match Cargo.toml",
        ),
        Ok(failures) => PublishStatusCheck::blocked("npm_metadata", failures.join("; ")),
        Err(error) => PublishStatusCheck::blocked("npm_metadata", error.to_string()),
    }
}

fn npm_token_status(env: &impl Fn(&str) -> Option<String>) -> PublishStatusCheck {
    if env("NPM_TOKEN").is_some() || env("NODE_AUTH_TOKEN").is_some() {
        PublishStatusCheck::ready("npm_token", "NPM_TOKEN or NODE_AUTH_TOKEN is configured")
    } else {
        PublishStatusCheck::blocked(
            "npm_token",
            "NPM_TOKEN/NODE_AUTH_TOKEN is missing; tag workflow will skip npm publish",
        )
    }
}

fn npm_artifacts_status(npm_dist: Option<&str>, version: &str) -> PublishStatusCheck {
    let Some(npm_dist) = npm_dist else {
        return PublishStatusCheck::skipped(
            "npm_artifacts",
            "pass --npm-dist <dir> after release matrix npm artifacts are downloaded",
        );
    };
    let npm_dist = Path::new(npm_dist);
    if !npm_dist.is_dir() {
        return PublishStatusCheck::blocked(
            "npm_artifacts",
            format!("npm artifact directory not found: {}", npm_dist.display()),
        );
    }

    let mut missing = Vec::new();
    for platform in NPM_PLATFORMS {
        let file = format!("deepseek-code-cli-{platform}-{version}.tgz");
        if !npm_dist.join(&file).is_file() {
            missing.push(file);
        }
    }
    if missing.is_empty() {
        PublishStatusCheck::ready(
            "npm_artifacts",
            "platform npm package tarballs are present for Linux, macOS, and Windows",
        )
    } else {
        PublishStatusCheck::blocked(
            "npm_artifacts",
            format!(
                "missing platform npm package tarball(s): {}",
                missing.join(", ")
            ),
        )
    }
}

fn release_asset_status(dist: Option<&str>) -> PublishStatusCheck {
    let Some(dist) = dist else {
        return PublishStatusCheck::skipped(
            "release_assets",
            "pass --dist <dir> after release archives and .sha256 files are downloaded",
        );
    };
    let dist = Path::new(dist);
    if !dist.is_dir() {
        return PublishStatusCheck::blocked(
            "release_assets",
            format!("release asset directory not found: {}", dist.display()),
        );
    }

    let expected = [
        "deepseek-linux-x64.tar.gz",
        "deepseek-macos-x64.tar.gz",
        "deepseek-macos-arm64.tar.gz",
        "deepseek-windows-x64.zip",
    ];
    let mut missing = Vec::new();
    let mut invalid = Vec::new();
    for artifact in expected {
        if !dist.join(artifact).is_file() {
            missing.push(artifact.to_string());
        }
        let sha_file = format!("{artifact}.sha256");
        match read_sha256_file(&dist.join(&sha_file)) {
            Ok(sha) if sha != "0".repeat(64) => {}
            Ok(_) => invalid.push(format!("{sha_file}: placeholder zero checksum")),
            Err(error) => invalid.push(error.to_string()),
        }
    }

    if missing.is_empty() && invalid.is_empty() {
        PublishStatusCheck::ready(
            "release_assets",
            "release archives and non-placeholder SHA-256 files are present for every platform",
        )
    } else {
        let mut details = Vec::new();
        if !missing.is_empty() {
            details.push(format!("missing archive(s): {}", missing.join(", ")));
        }
        if !invalid.is_empty() {
            details.push(format!("invalid checksum(s): {}", invalid.join("; ")));
        }
        PublishStatusCheck::blocked("release_assets", details.join("; "))
    }
}

fn homebrew_template_status(repo: &Path, version: &str) -> PublishStatusCheck {
    let path = repo.join("packaging/homebrew/deepseek.rb");
    match std::fs::read_to_string(&path) {
        Ok(formula) => {
            let formula_version = ruby_string_value(&formula, "version");
            if formula_version.as_deref() != Some(version) {
                return PublishStatusCheck::blocked(
                    "homebrew_formula",
                    format!(
                        "formula version {} does not match Cargo.toml {version}",
                        formula_version.unwrap_or_else(|| "<missing>".to_string())
                    ),
                );
            }
            PublishStatusCheck::ready(
                "homebrew_formula",
                "tracked formula template matches the package version; tap publish renders real checksums from --dist",
            )
        }
        Err(error) => PublishStatusCheck::blocked(
            "homebrew_formula",
            format!("failed to read {}: {error}", path.display()),
        ),
    }
}

fn homebrew_tap_status(env: &impl Fn(&str) -> Option<String>) -> PublishStatusCheck {
    let tap = env("HOMEBREW_TAP_REPOSITORY");
    let token = env("HOMEBREW_TAP_TOKEN");
    match (tap, token) {
        (Some(tap), Some(_)) if is_owner_repo(&tap) => PublishStatusCheck::ready(
            "homebrew_tap",
            format!("HOMEBREW_TAP_REPOSITORY is configured as {tap}"),
        ),
        (Some(tap), Some(_)) => PublishStatusCheck::blocked(
            "homebrew_tap",
            format!("HOMEBREW_TAP_REPOSITORY must use owner/name, got {tap}"),
        ),
        _ => PublishStatusCheck::blocked(
            "homebrew_tap",
            "HOMEBREW_TAP_REPOSITORY/HOMEBREW_TAP_TOKEN is missing; tag workflow will skip tap publish",
        ),
    }
}

const NPM_PLATFORMS: &[&str] = &["linux-x64", "macos-arm64", "macos-x64", "windows-x64"];

fn npm_metadata_failures(repo: &Path, version: &str) -> AppResult<Vec<String>> {
    let npm_root = repo.join("npm");
    let root_package = std::fs::read_to_string(npm_root.join("package.json"))?;
    let mut failures = Vec::new();

    let root_version = json_string_value(&root_package, "version");
    if root_version.as_deref() != Some(version) {
        failures.push(format!(
            "npm/package.json version {} does not match Cargo.toml {version}",
            root_version.unwrap_or_else(|| "<missing>".to_string())
        ));
    }
    if json_string_value(&root_package, "license").as_deref() != Some("SEE LICENSE IN LICENSE") {
        failures.push("npm/package.json license should be SEE LICENSE IN LICENSE".to_string());
    }
    if !npm_root.join("LICENSE").is_file() {
        failures.push("npm/LICENSE is missing".to_string());
    }

    for platform in NPM_PLATFORMS {
        let path = npm_root
            .join("platforms")
            .join(platform)
            .join("package.json");
        let package = std::fs::read_to_string(&path)?;
        let expected_name = format!("@deepseek-code/cli-{platform}");
        let name = json_string_value(&package, "name");
        if name.as_deref() != Some(expected_name.as_str()) {
            failures.push(format!(
                "{} name {} does not match {expected_name}",
                path.display(),
                name.unwrap_or_else(|| "<missing>".to_string())
            ));
        }
        let package_version = json_string_value(&package, "version");
        if package_version.as_deref() != Some(version) {
            failures.push(format!(
                "{expected_name} version {} does not match Cargo.toml {version}",
                package_version.unwrap_or_else(|| "<missing>".to_string())
            ));
        }
        if json_string_value(&package, "license").as_deref() != Some("SEE LICENSE IN LICENSE") {
            failures.push(format!(
                "{expected_name} license should be SEE LICENSE IN LICENSE"
            ));
        }
        if !npm_root
            .join("platforms")
            .join(platform)
            .join("LICENSE")
            .is_file()
        {
            failures.push(format!("{expected_name} LICENSE file is missing"));
        }

        let optional_version = json_dependency_version(&root_package, &expected_name);
        if optional_version.as_deref() != Some(version) {
            failures.push(format!(
                "npm/package.json optionalDependency {expected_name}={} does not match {version}",
                optional_version.unwrap_or_else(|| "<missing>".to_string())
            ));
        }
    }

    Ok(failures)
}

fn env_value(name: &str) -> Option<String> {
    std::env::var_os(name)
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn repository_slug(cargo_toml: &str, env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    env("GITHUB_REPOSITORY")
        .filter(|value| is_owner_repo(value))
        .or_else(|| {
            toml_string_value(cargo_toml, "repository")
                .and_then(|value| repository_slug_from_url(&value))
        })
}

fn repository_slug_from_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let candidate = without_git
        .strip_prefix("https://github.com/")
        .or_else(|| without_git.strip_prefix("http://github.com/"))
        .or_else(|| without_git.strip_prefix("git@github.com:"))
        .or_else(|| without_git.strip_prefix("ssh://git@github.com/"))
        .unwrap_or(without_git);

    if is_owner_repo(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn toml_string_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((left, right)) = line.split_once('=') else {
            continue;
        };
        if left.trim() == key {
            return parse_quoted_string(right.trim());
        }
    }
    None
}

fn toml_bool_value(content: &str, key: &str) -> Option<bool> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((left, right)) = line.split_once('=') else {
            continue;
        };
        if left.trim() == key {
            return match right.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        }
    }
    None
}

fn json_string_value(content: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let index = content.find(&needle)?;
    let after_key = &content[index + needle.len()..];
    let after_colon = after_key.split_once(':')?.1.trim_start();
    parse_quoted_string(after_colon)
}

fn json_dependency_version(content: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\"");
    let index = content.find(&needle)?;
    let after_key = &content[index + needle.len()..];
    let after_colon = after_key.split_once(':')?.1.trim_start();
    parse_quoted_string(after_colon)
}

fn ruby_string_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            return parse_quoted_string(rest.trim_start());
        }
    }
    None
}

fn parse_quoted_string(value: &str) -> Option<String> {
    let value = value.trim_start();
    let rest = value.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn is_owner_repo(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repo) = parts.next() else {
        return false;
    };
    parts.next().is_none() && !owner.is_empty() && !repo.is_empty()
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
task and live RLM worker daemon (`deepseek agents daemon --json`), the
diagnostics watch worker (`deepseek diagnostics --watch --changed --json`), and
the workspace shell supervisor protocol bridge
(`deepseek agents shell-supervisor --json`). The shell supervisor currently
publishes workspace-local status/show/start/wait/replay/attach/stdin/resize/cancel
over the socket and controls durable safe shell jobs, including supervisor-owned
native PTY sessions on Linux. Use `deepseek agents shell ...` as the human CLI
wrapper for those protocol controls.
The agents daemon triggers due automations, executes pending runtime tasks,
recovers stale live RLM ownership, and runs one queued live RLM turn per tick.
Review the generated WorkingDirectory, bind address, poll interval, and budget
before installing the files with systemd or launchd. The rendered output also
writes a workspace-specific `SERVICES.md` with install, start, status, log,
restart, stop, disable/unload, and runtime health-check commands.

Useful RLM service checks after startup:

```bash
curl -fsS http://127.0.0.1:13000/v1/health
deepseek doctor --json
deepseek agents rlm-status --json
deepseek agents rlm-events <session_id> --cursor 0 --json
deepseek agents rlm-wait <session_id> --cursor 0 --timeout-ms 5000 --json
deepseek agents shell status --json
```
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
            "systemd/deepseek-shell-supervisor.service",
            include_str!("../../../packaging/systemd/deepseek-shell-supervisor.service"),
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
        (
            "launchd/com.deepseek.shell-supervisor.plist",
            include_str!("../../../packaging/launchd/com.deepseek.shell-supervisor.plist"),
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
    fn publish_status_reports_missing_external_publish_inputs() {
        let args = UpdatePublishStatusArgs::default();
        let report = build_publish_status_report(&repo_root(), &args, &|_| None).unwrap();

        assert_eq!(status_of(&report, "cargo_registry"), PublishStatus::Ready);
        assert_eq!(status_of(&report, "npm_metadata"), PublishStatus::Ready);
        assert_eq!(status_of(&report, "npm_token"), PublishStatus::Blocked);
        assert_eq!(status_of(&report, "npm_artifacts"), PublishStatus::Skipped);
        assert_eq!(status_of(&report, "release_assets"), PublishStatus::Skipped);
        assert_eq!(status_of(&report, "homebrew_formula"), PublishStatus::Ready);
        assert_eq!(status_of(&report, "homebrew_tap"), PublishStatus::Blocked);
        assert_eq!(report.repository, "willamhou/DeepSeekCode");
        assert_eq!(
            public_status_of(&report, "source_checkout"),
            PublicInstallStatus::SourceAvailable
        );
        assert_eq!(
            public_status_of(&report, "npm"),
            PublicInstallStatus::RequiresPublish
        );
        assert_eq!(
            public_status_of(&report, "homebrew"),
            PublicInstallStatus::RequiresPublish
        );
        assert_eq!(
            public_status_of(&report, "cargo_registry"),
            PublicInstallStatus::SourceOnlyPolicy
        );
        assert_eq!(report.not_ready_count(), 4);
    }

    #[test]
    fn publish_status_passes_when_publish_artifacts_and_env_are_present() {
        let root = temp_root("publish-ready");
        let dist = root.join("dist-assets");
        let npm_dist = root.join("npm-dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::create_dir_all(&npm_dist).unwrap();

        for (index, artifact) in [
            "deepseek-linux-x64.tar.gz",
            "deepseek-macos-x64.tar.gz",
            "deepseek-macos-arm64.tar.gz",
            "deepseek-windows-x64.zip",
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(dist.join(artifact), "archive").unwrap();
            let sha_char = char::from(b'a' + index as u8);
            std::fs::write(
                dist.join(format!("{artifact}.sha256")),
                format!("{}  {artifact}\n", sha_char.to_string().repeat(64)),
            )
            .unwrap();
        }

        let version = env!("CARGO_PKG_VERSION");
        for platform in NPM_PLATFORMS {
            std::fs::write(
                npm_dist.join(format!("deepseek-code-cli-{platform}-{version}.tgz")),
                "npm package",
            )
            .unwrap();
        }

        let args = UpdatePublishStatusArgs {
            dist: Some(dist.display().to_string()),
            npm_dist: Some(npm_dist.display().to_string()),
            strict: true,
            json: false,
        };
        let report = build_publish_status_report(&repo_root(), &args, &|name| match name {
            "NPM_TOKEN" => Some("npm-token".to_string()),
            "HOMEBREW_TAP_REPOSITORY" => Some("owner/homebrew-tap".to_string()),
            "HOMEBREW_TAP_TOKEN" => Some("tap-token".to_string()),
            _ => None,
        })
        .unwrap();

        assert_eq!(report.not_ready_count(), 0);
        assert_eq!(status_of(&report, "release_assets"), PublishStatus::Ready);
        assert_eq!(status_of(&report, "npm_artifacts"), PublishStatus::Ready);
        assert_eq!(status_of(&report, "homebrew_tap"), PublishStatus::Ready);
        assert_eq!(
            public_status_of(&report, "github_release"),
            PublicInstallStatus::ReadyToPublish
        );
        assert_eq!(
            public_status_of(&report, "npm"),
            PublicInstallStatus::ReadyToPublish
        );
        assert_eq!(
            public_status_of(&report, "homebrew"),
            PublicInstallStatus::ReadyToPublish
        );
    }

    #[test]
    fn render_publish_status_json_includes_blockers() {
        let args = UpdatePublishStatusArgs::default();
        let report = build_publish_status_report(&repo_root(), &args, &|_| None).unwrap();

        let json = render_publish_status_json(&report, true);

        assert!(json.contains("\"kind\":\"deepseek.publish_status.v1\""));
        assert!(json.contains("\"version\":\""));
        assert!(json.contains("\"strict\":true"));
        assert!(json.contains("\"not_ready\":4"));
        assert!(json.contains("\"name\":\"npm_token\""));
        assert!(json.contains("\"status\":\"blocked\""));
        assert!(json.contains("NPM_TOKEN/NODE_AUTH_TOKEN is missing"));
        assert!(json.contains("\"public_install\""));
        assert!(json.contains("\"status\":\"requires_publish\""));
        assert!(json.contains("npm view @deepseek-code/cli version"));
    }

    #[test]
    fn release_asset_status_rejects_placeholder_checksums() {
        let root = temp_root("publish-placeholder-sha");
        std::fs::create_dir_all(&root).unwrap();
        for artifact in [
            "deepseek-linux-x64.tar.gz",
            "deepseek-macos-x64.tar.gz",
            "deepseek-macos-arm64.tar.gz",
            "deepseek-windows-x64.zip",
        ] {
            std::fs::write(root.join(artifact), "archive").unwrap();
            std::fs::write(
                root.join(format!("{artifact}.sha256")),
                format!("{}  {artifact}\n", "0".repeat(64)),
            )
            .unwrap();
        }

        let check = release_asset_status(Some(&root.display().to_string()));

        assert_eq!(check.status, PublishStatus::Blocked);
        assert!(check.detail.contains("placeholder zero checksum"));
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
            .join("systemd/deepseek-shell-supervisor.service")
            .is_file());
        assert!(package
            .service_templates
            .join("launchd/com.deepseek.agents.plist")
            .is_file());
        assert!(package
            .service_templates
            .join("launchd/com.deepseek.diagnostics.plist")
            .is_file());
        assert!(package
            .service_templates
            .join("launchd/com.deepseek.shell-supervisor.plist")
            .is_file());
        let packaged_agents_service = std::fs::read_to_string(
            package
                .service_templates
                .join("systemd/deepseek-agents.service"),
        )
        .unwrap();
        assert!(packaged_agents_service.contains("queued live RLM turn per tick"));
        let packaged_agents_plist = std::fs::read_to_string(
            package
                .service_templates
                .join("launchd/com.deepseek.agents.plist"),
        )
        .unwrap();
        assert!(packaged_agents_plist.contains("queued live RLM turn per tick"));
        let packaged_shell_supervisor_service = std::fs::read_to_string(
            package
                .service_templates
                .join("systemd/deepseek-shell-supervisor.service"),
        )
        .unwrap();
        assert!(packaged_shell_supervisor_service.contains("agents shell-supervisor --json"));
        let packaged_shell_supervisor_plist = std::fs::read_to_string(
            package
                .service_templates
                .join("launchd/com.deepseek.shell-supervisor.plist"),
        )
        .unwrap();
        assert!(packaged_shell_supervisor_plist.contains("<string>shell-supervisor</string>"));
        assert!(package.package_dir.join("VERIFY.md").is_file());
        let manifest = std::fs::read_to_string(package.manifest).unwrap();
        assert!(manifest.contains("\"name\": \"deepseek\""));
        assert!(manifest.contains("\"version\":"));
        let services = std::fs::read_to_string(package.services_doc).unwrap();
        assert!(services.contains("deepseek agents service"));
        assert!(services.contains("workspace-specific `SERVICES.md`"));
        assert!(services.contains("live RLM worker daemon"));
        assert!(services.contains("deepseek agents shell-supervisor --json"));
        assert!(services.contains("curl -fsS http://127.0.0.1:13000/v1/health"));
        assert!(services.contains("deepseek agents rlm-status --json"));
        assert!(services.contains("deepseek agents shell status --json"));
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

    fn status_of(report: &PublishStatusReport, name: &str) -> PublishStatus {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("missing check {name}"))
            .status
    }

    fn public_status_of(report: &PublishStatusReport, name: &str) -> PublicInstallStatus {
        report
            .public_install
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("missing public install check {name}"))
            .status
    }
}
