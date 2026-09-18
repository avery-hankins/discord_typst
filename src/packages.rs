use std::borrow::Cow;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;

use typst::diag::{FileError, FileResult};
use typst::foundations::Bytes;
use typst::syntax::{FileId, Source, VirtualRoot, package::PackageSpec};
use typst_as_lib::cached_file_resolver::IntoCachedFileResolver;
use typst_as_lib::file_resolver::{FileResolver, FileSystemResolver};

/// Vendored packages, in the layout `<root>/<namespace>/<name>/<version>/`.
/// Overridable so a deployed binary doesn't have to run from the repo root.
static PACKAGE_ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::var_os("TYPST_PACKAGES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("packages"))
});

const PACKAGE_LIST: &str = include_str!("../packages/packages.txt");

pub static ALLOWED: LazyLock<Vec<AllowedPackage>> =
    LazyLock::new(|| parse_package_list(PACKAGE_LIST));

pub struct AllowedPackage {
    pub spec: PackageSpec,
    pub listing: Option<Listing>,
}

pub struct Listing {
    pub display_name: &'static str,
    pub description: &'static str,
}

impl AllowedPackage {
    /// The import path a user writes, e.g. `@preview/cetz:0.5.2`.
    pub fn import_path(&self) -> String {
        self.spec.to_string()
    }

    pub fn universe_url(&self) -> String {
        format!("https://typst.app/universe/package/{}", self.spec.name)
    }
}

/// Parses `packages/packages.txt`. Panics on malformed line: the file is
/// embedded at compile time, so anything wrong with it is a build error
/// caught at program startup.
pub(crate) fn parse_package_list(list: &'static str) -> Vec<AllowedPackage> {
    let mut packages = Vec::new();
    for line in list.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split('|').map(str::trim);
        let spec = fields.next().unwrap_or("");
        let spec = PackageSpec::from_str(spec)
            .unwrap_or_else(|e| panic!("bad package spec in packages.txt: {line:?}: {e}"));

        let listing = match (fields.next(), fields.next()) {
            (Some(display_name), Some(description)) => Some(Listing {
                display_name,
                description,
            }),
            (None, _) => None,
            _ => panic!("package listing in packages.txt needs a description: {line:?}"),
        };

        packages.push(AllowedPackage { spec, listing });
    }
    packages
}

/// Parses the package list eagerly so a malformed entry fails at startup.
pub fn force_loaded() {
    LazyLock::force(&ALLOWED);
}

/// Packages advertised by `/typstpackages`, in the order they are listed.
pub fn listed() -> impl Iterator<Item = (&'static AllowedPackage, &'static Listing)> {
    ALLOWED
        .iter()
        .filter_map(|p| p.listing.as_ref().map(|l| (p, l)))
}

/// A file resolver that only reaches whitelisted packages.
///
/// User code runs untrusted, so anything outside a listed package - project
/// files, unlisted packages, other namespaces - is reported as missing.
pub struct WhitelistedPackages<R>(R);

impl<R> WhitelistedPackages<R> {
    fn check(&self, id: FileId) -> FileResult<()> {
        match id.root() {
            VirtualRoot::Package(spec) if ALLOWED.iter().any(|p| p.spec == *spec) => Ok(()),
            _ => Err(FileError::NotFound(id.vpath().get_without_slash().into())),
        }
    }
}

impl<R: FileResolver> FileResolver for WhitelistedPackages<R> {
    fn resolve_binary(&self, id: FileId) -> FileResult<Cow<'_, Bytes>> {
        self.check(id)?;
        self.0.resolve_binary(id)
    }

    fn resolve_source(&self, id: FileId) -> FileResult<Cow<'_, Source>> {
        self.check(id)?;
        self.0.resolve_source(id)
    }
}

/// Builds the resolver to hand to the Typst engine.
pub fn resolver() -> impl FileResolver + Send + Sync + 'static {
    // `new` takes the project root, which the whitelist always rejects; only
    // the package root is ever read from.
    WhitelistedPackages(
        FileSystemResolver::new(PACKAGE_ROOT.clone())
            .local_package_root(PACKAGE_ROOT.clone())
            .into_cached(),
    )
}
