use std::borrow::Cow;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use typst::diag::{FileError, FileResult};
use typst::foundations::Bytes;
use typst::syntax::{FileId, Source, VirtualRoot, package::PackageSpec};
use typst_as_lib::cached_file_resolver::IntoCachedFileResolver;
use typst_as_lib::file_resolver::{FileResolver, FileSystemResolver};
use typst_as_lib::package_resolver::{InMemoryCache, PackageResolver};

/// Vendored packages, in the layout `<root>/<namespace>/<name>/<version>/`.
/// Overridable so a deployed binary doesn't have to run from the repo root.
static PACKAGE_ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::var_os("TYPST_PACKAGES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("packages"))
});

const PACKAGE_LIST: &str = include_str!("../packages/packages.txt");

pub static VENDORED: LazyLock<Vec<VendoredPackage>> =
    LazyLock::new(|| parse_package_list(PACKAGE_LIST));

pub struct VendoredPackage {
    pub spec: PackageSpec,
    pub listing: Option<Listing>,
}

pub struct Listing {
    pub display_name: &'static str,
    pub description: &'static str,
}

impl VendoredPackage {
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
pub(crate) fn parse_package_list(list: &'static str) -> Vec<VendoredPackage> {
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

        packages.push(VendoredPackage { spec, listing });
    }
    packages
}

/// Parses the package list eagerly so a malformed entry fails at startup.
pub fn force_loaded() {
    LazyLock::force(&VENDORED);
}

/// Packages advertised by `/typstpackages`, in the order they are listed.
pub fn listed() -> impl Iterator<Item = (&'static VendoredPackage, &'static Listing)> {
    VENDORED
        .iter()
        .filter_map(|p| p.listing.as_ref().map(|l| (p, l)))
}

/// Serves the packages vendored into `packages/` and nothing else.
///
/// Reporting everything else as missing is what lets the engine fall through
/// to [`network_resolver`], and it keeps project files off limits: user code
/// runs untrusted, and the network resolver only speaks to the registry.
pub struct VendoredResolver<R>(R);

/// Whether the package is on disk, and so resolvable without the network.
pub fn is_vendored(spec: &PackageSpec) -> bool {
    VENDORED.iter().any(|p| p.spec == *spec)
}

impl<R> VendoredResolver<R> {
    fn check(&self, id: FileId) -> FileResult<()> {
        match id.root() {
            VirtualRoot::Package(spec) if is_vendored(spec) => Ok(()),
            _ => Err(FileError::NotFound(id.vpath().get_without_slash().into())),
        }
    }
}

impl<R: FileResolver> FileResolver for VendoredResolver<R> {
    fn resolve_binary(&self, id: FileId) -> FileResult<Cow<'_, Bytes>> {
        self.check(id)?;
        self.0.resolve_binary(id)
    }

    fn resolve_source(&self, id: FileId) -> FileResult<Cow<'_, Source>> {
        self.check(id)?;
        self.0.resolve_source(id)
    }
}

/// Serves the vendored `packages/` tree.
pub fn vendored_resolver() -> impl FileResolver + Send + Sync + 'static {
    // `new` takes the project root, which `check` always rejects; only the
    // package root is ever read from.
    VendoredResolver(
        FileSystemResolver::new(PACKAGE_ROOT.clone())
            .local_package_root(PACKAGE_ROOT.clone())
            .into_cached(),
    )
}

/// How long a single registry request may take. The download happens inside
/// the render subprocess, so it comes out of MAX_COMPILE_SECONDS; keep it
/// short enough that a stalled registry still leaves time to render.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(15);
const REGISTRY_RETRIES: u32 = 2;

/// Serves from the Typst registry, for anything that the vendored packages don't serve.
///
/// The archive is unpacked into memory for the lifetime of this resolver,
/// which is a single render: nothing is written to disk and nothing is reused
/// across renders, so a package imported on every render is downloaded on
/// every render. typst-as-lib refuses any namespace other than `@preview`, so
/// the registry is the only host user code can reach.
pub fn network_resolver() -> PackageResolver<InMemoryCache> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REGISTRY_TIMEOUT))
        .timeout_connect(Some(REGISTRY_TIMEOUT))
        .build()
        .new_agent();

    PackageResolver::builder()
        .with_in_memory_cache()
        .ureq_agent(agent)
        .request_retry_count(REGISTRY_RETRIES)
        .build()
}
