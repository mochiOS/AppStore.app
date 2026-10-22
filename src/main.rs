#[cfg(target_os = "mochios")]
mod api;
#[cfg(not(target_os = "mochios"))]
#[path = "preview_api.rs"]
mod api;
mod catalog;

use api::{ApiError, DataRequest};
use catalog::{CatalogApp, CatalogRelease, ReleaseResponse, Storefront};
use mochios_package_protocol as package_protocol;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use viewkit::event::{EventContext, EventResult, ViewEvent};
use viewkit::prelude::*;
use viewkit::view::{Constraints, MeasureContext, PaintContext};

const LIBRARY: [&str; 2] = ["Updates", "Installed"];
const APP_ICON_RADIUS: f32 = 14.0;
const APP_TILE_WIDTH: f32 = 300.0;
const APP_TILE_HEIGHT: f32 = 100.0;
const APP_ICON_SIZE: f32 = 64.0;
const MAX_ICON_DIMENSION: u32 = 2_048;
const SELECTION_ALL: &str = "all";
const SELECTION_UPDATES: &str = "library:updates";
const SELECTION_INSTALLED: &str = "library:installed";
const STOREFRONT_REQUEST_PREFIX: u64 = 0x5354_4f52_0000_0000;
const RELEASE_REQUEST_PREFIX: u64 = 0x5245_4c53_0000_0000;
const PACKAGE_REQUEST_PREFIX: u64 = 0x504b_4744_0000_0000;
const REQUEST_KIND_MASK: u64 = 0xffff_ffff_0000_0000;
const REQUEST_SEQUENCE_MASK: u64 = 0x0000_0000_ffff_ffff;

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

type StoreView = NavigationSplitView<Scroll, VStack>;

#[derive(Default)]
struct StoreUi {
    search: TextFieldInteractionState,
    sidebar_scroll: ScrollState,
    browse_scroll: ScrollState,
    detail_scroll: ScrollState,
    buttons: RefCell<BTreeMap<String, ButtonInteractionState>>,
}
impl StoreUi {
    fn button(&self, key: impl Into<String>, label: impl Into<String>) -> Button {
        let interaction = self
            .buttons
            .borrow_mut()
            .entry(key.into())
            .or_default()
            .clone();
        Button::with_interaction_and_label(interaction, label)
    }
}

struct StoreSearch {
    field: TextField,
    query: State<String>,
    selected_app: State<Option<String>>,
    scroll: ScrollState,
    input: TextFieldInteractionState,
}
impl View for StoreSearch {
    fn measure(&self, constraints: Constraints, context: &mut MeasureContext<'_>) -> Size {
        self.field.measure(constraints, context)
    }
    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        self.field.paint(bounds, context);
    }
    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        let result = self.field.handle_event(bounds, event, context);
        let value = self.input.value();
        if value != self.query.get() {
            self.query.set(value);
            self.selected_app.set(None);
            self.scroll.reset();
        }
        result
    }
}

#[derive(Clone)]
struct PendingRelease {
    request_id: u64,
    bundle_id: String,
}

#[derive(Clone)]
struct PendingInstall {
    request_id: u64,
    release: CatalogRelease,
    path: String,
    offset: usize,
    hasher: Sha256,
    mutation: PackageMutation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageMutation {
    Install,
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledPackage {
    package_id: String,
    name: String,
    version: String,
    kind: String,
    built_in: bool,
    removable: bool,
}

#[derive(Clone)]
enum ReleaseStatus {
    Idle,
    Loading { bundle_id: String },
    Ready(CatalogRelease),
    Unavailable { bundle_id: String },
    Failed { bundle_id: String, message: String },
}

#[derive(Clone)]
enum InstallStatus {
    Idle,
    Downloading { bundle_id: String },
    Installing { bundle_id: String },
    Removing { bundle_id: String },
    Installed { bundle_id: String },
    Removed { bundle_id: String },
    Failed { bundle_id: String, message: String },
}

enum CatalogStatus {
    Loading,
    Ready(Storefront),
    Failed(String),
}

fn next_request_id(prefix: u64) -> u64 {
    prefix | (REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed) & REQUEST_SEQUENCE_MASK)
}

fn category_selection(name: &str) -> String {
    format!("category:{name}")
}

fn compatible_release(
    response: ReleaseResponse,
    expected_bundle_id: &str,
) -> Result<Option<CatalogRelease>, String> {
    if response.bundle_id != expected_bundle_id {
        return Err(String::from(
            "The release response did not match this application.",
        ));
    }
    let Some(release) = response.releases.into_iter().next() else {
        return Ok(None);
    };
    let valid_sha256 = release.sha256.len() == 64
        && release
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if release.bundle_id != expected_bundle_id
        || release.release_id.trim().is_empty()
        || release.version.trim().is_empty()
        || release.architecture.as_deref() != Some("x86_64")
        || release.abi.as_deref() != Some("mochios-1")
        || !(32..=api::MAX_PACKAGE_BYTES as i64).contains(&release.size)
        || !valid_sha256
    {
        return Err(String::from(
            "The App Store returned an invalid or incompatible release.",
        ));
    }
    Ok(Some(release))
}

fn version_is_newer(candidate: &str, installed: &str) -> bool {
    fn components(version: &str) -> Option<(Vec<u64>, Option<&str>)> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let (core, suffix) = version
            .split_once('-')
            .map_or((version, None), |(core, suffix)| (core, Some(suffix)));
        let numbers = core
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        (!numbers.is_empty()).then_some((numbers, suffix))
    }
    let (
        Some((mut candidate_numbers, candidate_suffix)),
        Some((mut installed_numbers, installed_suffix)),
    ) = (components(candidate), components(installed))
    else {
        return false;
    };
    let length = candidate_numbers.len().max(installed_numbers.len());
    candidate_numbers.resize(length, 0);
    installed_numbers.resize(length, 0);
    match candidate_numbers.cmp(&installed_numbers) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (candidate_suffix, installed_suffix) {
            (None, Some(_)) => true,
            (Some(candidate), Some(installed)) => candidate > installed,
            _ => false,
        },
    }
}

fn installed_package<'a>(
    packages: &'a [InstalledPackage],
    package_id: &str,
) -> Option<&'a InstalledPackage> {
    packages
        .iter()
        .find(|package| package.package_id == package_id)
}

fn set_release_result(
    bundle_id: &str,
    result: Result<ReleaseResponse, ApiError>,
    release_status: &State<ReleaseStatus>,
) {
    let status = match result {
        Ok(response) => match compatible_release(response, bundle_id) {
            Ok(Some(release)) => ReleaseStatus::Ready(release),
            Ok(None) => ReleaseStatus::Unavailable {
                bundle_id: bundle_id.to_string(),
            },
            Err(message) => ReleaseStatus::Failed {
                bundle_id: bundle_id.to_string(),
                message,
            },
        },
        Err(error) => ReleaseStatus::Failed {
            bundle_id: bundle_id.to_string(),
            message: error.to_string(),
        },
    };
    release_status.set(status);
}

fn open_app(
    bundle_id: &str,
    selected_app: &State<Option<String>>,
    release_status: &State<ReleaseStatus>,
    pending_release: &State<Option<PendingRelease>>,
) {
    selected_app.set(Some(bundle_id.to_string()));
    release_status.set(ReleaseStatus::Loading {
        bundle_id: bundle_id.to_string(),
    });
    let request_id = next_request_id(RELEASE_REQUEST_PREFIX);
    match api::start_release_request(request_id, bundle_id) {
        Ok(DataRequest::Pending) => pending_release.set(Some(PendingRelease {
            request_id,
            bundle_id: bundle_id.to_string(),
        })),
        Ok(DataRequest::Ready(response)) => {
            pending_release.set(None);
            set_release_result(bundle_id, Ok(response), release_status);
        }
        Err(error) => {
            pending_release.set(None);
            set_release_result(bundle_id, Err(error), release_status);
        }
    }
}

fn navigation(
    storefront: Option<&Storefront>,
    search: State<String>,
    selection: State<String>,
    selected_app: State<Option<String>>,
    ui: &Rc<StoreUi>,
) -> VStack {
    let all_selection = selection.clone();
    let all_selected_app = selected_app.clone();
    let all_search = search.clone();
    let all_ui = ui.clone();
    let mut browse = SidebarSection::new("Explore").item(
        SidebarItem::new("Discover")
            .symbol(SymbolName::Grid)
            .selected(selection.get() == SELECTION_ALL && search.get().trim().is_empty())
            .on_select(move || {
                all_search.set(String::new());
                all_ui.search.set_value(String::new());
                all_ui.browse_scroll.reset();
                all_selection.set(String::from(SELECTION_ALL));
                all_selected_app.set(None);
            }),
    );
    if let Some(storefront) = storefront {
        for category in &storefront.categories {
            let target = category_selection(&category.name);
            let category_selection = selection.clone();
            let category_selected_app = selected_app.clone();
            let category_search = search.clone();
            let category_ui = ui.clone();
            browse = browse.item(
                SidebarItem::new(category.name.clone())
                    .symbol(SymbolName::Tag)
                    .selected(selection.get() == target && search.get().trim().is_empty())
                    .on_select(move || {
                        category_search.set(String::new());
                        category_ui.search.set_value(String::new());
                        category_ui.browse_scroll.reset();
                        category_selection.set(target.clone());
                        category_selected_app.set(None);
                    }),
            );
        }
    }
    let mut library = SidebarSection::new("Library");
    for label in LIBRARY {
        let (symbol, target) = match label {
            "Updates" => (SymbolName::ArrowDownCircle, SELECTION_UPDATES),
            "Installed" => (SymbolName::Internaldrive, SELECTION_INSTALLED),
            _ => continue,
        };
        let library_selection = selection.clone();
        let library_selected_app = selected_app.clone();
        let library_search = search.clone();
        let library_ui = ui.clone();
        library = library.item(
            SidebarItem::new(label)
                .symbol(symbol)
                .selected(selection.get() == target && search.get().trim().is_empty())
                .on_select(move || {
                    library_search.set(String::new());
                    library_ui.search.set_value(String::new());
                    library_ui.browse_scroll.reset();
                    library_selection.set(String::from(target));
                    library_selected_app.set(None);
                }),
        );
    }

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::ExtraLarge)
        .child(
            Padding::only(0.0, 0.0, 0.0, Theme::current().spacing.small)
                .content(Text::styled("App Store", TextRole::TitleSmall)),
        )
        .child(
            StoreSearch {
                field: TextField::with_interaction(ui.search.clone())
                    .placeholder("Search Apps")
                    .leading_symbol(SymbolName::Search),
                input: ui.search.clone(),
                query: search,
                selected_app,
                scroll: ui.browse_scroll.clone(),
            }
            .height(Theme::current().controls.height),
        )
        .child(browse)
        .child(library)
}

fn default_app_icon() -> Option<SvgData> {
    static ICON: OnceLock<Option<SvgData>> = OnceLock::new();
    ICON.get_or_init(|| SvgData::decode(include_bytes!("../../binder/resources/appicon.svg")).ok())
        .clone()
}

fn app_tile(
    app: &CatalogApp,
    icons: &[LoadedIcon],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    installed_packages: &[InstalledPackage],
    ui: &Rc<StoreUi>,
) -> Button {
    let installed = installed_package(installed_packages, &app.bundle_id).is_some();
    let subtitle = app
        .subtitle
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&app.category);
    let bundle_id = app.bundle_id.clone();
    let scroll = ui.detail_scroll.clone();
    ui.button(format!("app:{}", app.bundle_id), app.name.clone())
        .style(ButtonStyle::Ghost)
        .alignment(ZStackAlignment::Leading)
        .accessibility_label(format!("View {} by {}", app.name, app.developer))
        .content(
            Padding::symmetric(
                Theme::current().spacing.small,
                Theme::current().spacing.small,
            )
            .content(
                HStack::new()
                    .alignment(StackAlignment::Center)
                    .gap(StackGap::Medium)
                    .child(app_icon(app, icons, APP_ICON_SIZE))
                    .child(
                        VStack::new()
                            .alignment(StackAlignment::Stretch)
                            .gap(StackGap::ExtraSmall)
                            .child(Text::label(app.name.clone()).weight(600))
                            .child(Text::caption(subtitle).tone(TextTone::Secondary))
                            .child(
                                Text::caption(format!(
                                    "{}{}",
                                    app.developer,
                                    if installed { " · Installed" } else { "" }
                                ))
                                .tone(TextTone::Secondary),
                            )
                            .layout()
                            .flex_grow(1.0)
                            .flex_shrink(1.0),
                    )
                    .child(
                        Icon::new(SymbolName::ChevronRight)
                            .size(Theme::current().layout.compact_icon_size)
                            .color(Theme::current().colors.text_tertiary),
                    ),
            ),
        )
        .on_click(move || {
            scroll.reset();
            open_app(&bundle_id, &selected_app, &release_status, &pending_release);
        })
}

fn app_matches_search(app: &CatalogApp, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || app.name.to_ascii_lowercase().contains(&query)
        || app.developer.to_ascii_lowercase().contains(&query)
        || app.description.to_ascii_lowercase().contains(&query)
        || app
            .subtitle
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(&query)
        || app.bundle_id.to_ascii_lowercase().contains(&query)
}

fn grouped_apps<'a>(
    storefront: &'a Storefront,
    query: &str,
    category: Option<&str>,
) -> Vec<(String, Vec<&'a CatalogApp>)> {
    let mut groups: Vec<(String, Vec<&CatalogApp>)> = Vec::new();
    let mut seen = HashSet::new();
    for section in &storefront.sections {
        for app in &section.apps {
            if category.is_some_and(|category| app.category != category)
                || !app_matches_search(app, query)
                || !seen.insert(app.bundle_id.as_str())
            {
                continue;
            }
            let title = if app.category.trim().is_empty() {
                section.title.clone()
            } else {
                app.category.clone()
            };
            if let Some((_, apps)) = groups.iter_mut().find(|(name, _)| name == &title) {
                apps.push(app);
            } else {
                groups.push((title, vec![app]));
            }
        }
    }
    groups
}

fn section_view(
    title: String,
    subtitle: Option<&str>,
    apps: Vec<&CatalogApp>,
    icons: &[LoadedIcon],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    installed_packages: &[InstalledPackage],
    ui: &Rc<StoreUi>,
) -> VStack {
    let mut heading = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::ExtraSmall)
        .child(Text::styled(title, TextRole::TitleSmall));
    if let Some(subtitle) = subtitle.filter(|s| !s.trim().is_empty()) {
        heading = heading.child(Text::caption(subtitle).tone(TextTone::Secondary));
    }
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(heading)
        .child(
            AdaptiveGrid::new(APP_TILE_WIDTH, APP_TILE_HEIGHT)
                .fill_columns()
                .spacing(
                    Theme::current().spacing.large,
                    Theme::current().spacing.small,
                )
                .children(apps.into_iter().map(|app| {
                    app_tile(
                        app,
                        icons,
                        selected_app.clone(),
                        release_status.clone(),
                        pending_release.clone(),
                        installed_packages,
                        ui,
                    )
                })),
        )
}

fn catalog_content(
    storefront: &Storefront,
    query: &str,
    selection: &str,
    icons: &[LoadedIcon],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    installed_packages: &[InstalledPackage],
    ui: &Rc<StoreUi>,
) -> VStack {
    let searching = !query.trim().is_empty();
    let category = if searching {
        None
    } else {
        selection.strip_prefix("category:")
    };
    let groups = grouped_apps(storefront, query, category);
    let count: usize = groups.iter().map(|(_, apps)| apps.len()).sum();
    let title = if searching {
        "Search Results"
    } else {
        category.unwrap_or("Discover")
    };
    let noun = if count == 1 {
        "application"
    } else {
        "applications"
    };
    let subtitle = if searching {
        format!("{count} {noun} for “{}”", query.trim())
    } else if category.is_some() {
        format!("{count} {noun}")
    } else {
        String::from("Applications for your mochiOS desktop")
    };
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::ExtraLarge)
        .child(
            VStack::new()
                .alignment(StackAlignment::Stretch)
                .gap(StackGap::ExtraSmall)
                .child(Text::styled(title, TextRole::TitleMedium))
                .child(Text::caption(subtitle).tone(TextTone::Secondary)),
        );
    if !searching && category.is_none() {
        let mut seen = HashSet::new();
        for section in &storefront.sections {
            let apps: Vec<_> = section
                .apps
                .iter()
                .filter(|app| seen.insert(app.bundle_id.as_str()))
                .collect();
            if apps.is_empty() {
                continue;
            }
            content = content.child(section_view(
                section.title.clone(),
                section.subtitle.as_deref(),
                apps,
                icons,
                selected_app.clone(),
                release_status.clone(),
                pending_release.clone(),
                installed_packages,
                ui,
            ));
        }
    } else {
        for (title, apps) in groups.iter() {
            content = content.child(section_view(
                title.clone(),
                None,
                apps.clone(),
                icons,
                selected_app.clone(),
                release_status.clone(),
                pending_release.clone(),
                installed_packages,
                ui,
            ));
        }
    }
    if groups.is_empty() {
        content = content.child(
            Padding::symmetric(0.0, Theme::current().spacing.extra_large).content(
                VStack::new()
                    .alignment(StackAlignment::Stretch)
                    .gap(StackGap::Small)
                    .child(
                        Text::label(if searching {
                            "No matching applications"
                        } else {
                            "No applications available"
                        })
                        .weight(600),
                    )
                    .child(
                        Text::caption(if searching {
                            "Try an application name, developer, or a different search."
                        } else {
                            "Applications will appear here when they are available in the catalog."
                        })
                        .tone(TextTone::Secondary),
                    ),
            ),
        );
    }
    content
}

fn catalog_app<'a>(storefront: &'a Storefront, bundle_id: &str) -> Option<&'a CatalogApp> {
    storefront
        .sections
        .iter()
        .flat_map(|section| section.apps.iter())
        .find(|app| app.bundle_id == bundle_id)
}

fn app_icon(app: &CatalogApp, icons: &[LoadedIcon], size: f32) -> StackChild {
    if let Some(icon) = icons
        .iter()
        .find(|icon| icon.bundle_id == app.bundle_id)
        .map(|icon| icon.image.clone())
    {
        Image::new(icon)
            .content_mode(ImageContentMode::Fill)
            .radius(CornerRadius::Custom(APP_ICON_RADIUS))
            .accessibility_label(format!("{} icon", app.name))
            .frame(size, size)
    } else if let Some(icon) = default_app_icon() {
        Svg::new(icon)
            .content_mode(SvgContentMode::Fill)
            .radius(CornerRadius::Custom(APP_ICON_RADIUS))
            .accessibility_label(format!("{} icon", app.name))
            .frame(size, size)
    } else {
        Spacer::new().into_stack_child().frame(size, size)
    }
}

fn package_digest_matches(bytes: &[u8], expected: &str) -> bool {
    if expected.len() != 64 {
        return false;
    }
    let digest = Sha256::digest(bytes);
    digest_matches(&digest, expected)
}

fn digest_matches(digest: &[u8], expected: &str) -> bool {
    if digest.len() != 32 || expected.len() != 64 {
        return false;
    }
    let mut difference = 0u8;
    for (index, byte) in digest.iter().enumerate() {
        let high = hex_value(expected.as_bytes()[index * 2]);
        let low = hex_value(expected.as_bytes()[index * 2 + 1]);
        let Some(expected_byte) = high.zip(low).map(|(high, low)| (high << 4) | low) else {
            return false;
        };
        difference |= byte ^ expected_byte;
    }
    difference == 0
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn fail_install(
    pending: &PendingInstall,
    message: String,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
) {
    pending_install.set(None);
    let _ = fs::remove_file(&pending.path);
    install_status.set(InstallStatus::Failed {
        bundle_id: pending.release.bundle_id.clone(),
        message,
    });
}

fn finish_install(
    pending: PendingInstall,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
    installed_packages: &State<Vec<InstalledPackage>>,
) {
    pending_install.set(None);
    let digest = pending.hasher.finalize();
    if !digest_matches(&digest, &pending.release.sha256) {
        let _ = fs::remove_file(&pending.path);
        install_status.set(InstallStatus::Failed {
            bundle_id: pending.release.bundle_id,
            message: String::from("The downloaded package failed SHA-256 verification."),
        });
        return;
    }

    install_status.set(InstallStatus::Installing {
        bundle_id: pending.release.bundle_id.clone(),
    });
    let result = mutate_package(&pending.path, pending.mutation);
    let _ = fs::remove_file(&pending.path);
    match result {
        Ok(()) => match refresh_installed_packages(installed_packages) {
            Ok(()) => install_status.set(InstallStatus::Installed {
                bundle_id: pending.release.bundle_id,
            }),
            Err(message) => install_status.set(InstallStatus::Failed {
                bundle_id: pending.release.bundle_id,
                message,
            }),
        },
        Err(message) => install_status.set(InstallStatus::Failed {
            bundle_id: pending.release.bundle_id,
            message,
        }),
    }
}

fn consume_package_chunk(
    mut pending: PendingInstall,
    bytes: Vec<u8>,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
    installed_packages: &State<Vec<InstalledPackage>>,
) {
    let package_size = pending.release.size as usize;
    let expected = (package_size - pending.offset).min(api::MAX_PACKAGE_CHUNK_BYTES);
    if bytes.len() != expected {
        fail_install(
            &pending,
            String::from("The App Store returned an incomplete package range."),
            pending_install,
            install_status,
        );
        return;
    }
    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&pending.path)
            .map_err(|error| format!("Unable to reopen the package file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("Unable to save the package: {error}"))?;
        if pending.offset + bytes.len() == package_size {
            file.sync_all()
                .map_err(|error| format!("Unable to finish saving the package: {error}"))?;
        }
        Ok::<(), String>(())
    })();
    if let Err(message) = write_result {
        fail_install(&pending, message, pending_install, install_status);
        return;
    }
    pending.hasher.update(&bytes);
    pending.offset += bytes.len();
    if pending.offset == package_size {
        finish_install(pending, pending_install, install_status, installed_packages);
    } else {
        request_package_chunk(pending, pending_install, install_status, installed_packages);
    }
}

fn request_package_chunk(
    mut pending: PendingInstall,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
    installed_packages: &State<Vec<InstalledPackage>>,
) {
    let remaining = pending.release.size as usize - pending.offset;
    let length = remaining.min(api::MAX_PACKAGE_CHUNK_BYTES);
    pending.request_id = next_request_id(PACKAGE_REQUEST_PREFIX);
    match api::start_package_request(
        pending.request_id,
        &pending.release.bundle_id,
        &pending.release.version,
        pending.offset,
        length,
    ) {
        Ok(DataRequest::Pending) => pending_install.set(Some(pending)),
        Ok(DataRequest::Ready(bytes)) => {
            pending_install.set(None);
            consume_package_chunk(
                pending,
                bytes,
                pending_install,
                install_status,
                installed_packages,
            );
        }
        Err(error) => fail_install(&pending, error.to_string(), pending_install, install_status),
    }
}

#[cfg(target_os = "mochios")]
fn package_service_call(request: package_protocol::Request<'_>) -> Result<(), String> {
    let service =
        mochi_user_platform::process::find_by_name("package.service").map_err(|error| {
            format!(
                "Package service unavailable (errno {}).",
                error.errno().unwrap_or(0)
            )
        })?;
    if service == 0 {
        return Err(String::from("Package service is unavailable."));
    }
    let request_id = next_request_id(PACKAGE_REQUEST_PREFIX);
    let mut encoded = [0u8; package_protocol::HEADER_LEN + package_protocol::MAX_PATH_LEN];
    let request_length = package_protocol::encode_request(request_id, request, &mut encoded)
        .map_err(|_| String::from("The package request is invalid."))?;
    let mut reply = [0u8; package_protocol::HEADER_LEN];
    let message = mochi_user_platform::ipc::call(service, &encoded[..request_length], &mut reply)
        .map_err(|error| {
        format!(
            "Package operation failed (errno {}).",
            error.errno().unwrap_or(0)
        )
    })?;
    let reply_length = (message & 0xffff_ffff) as usize;
    let response = reply
        .get(..reply_length)
        .ok_or_else(|| String::from("Package service returned an invalid response."))?;
    let status = package_protocol::decode_status(response, request_id)
        .map_err(|_| String::from("Package service returned an invalid response."))?;
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "Package operation failed (errno {}).",
            status.unsigned_abs()
        ))
    }
}

#[cfg(target_os = "mochios")]
fn query_installed_packages() -> Result<Vec<InstalledPackage>, String> {
    let service =
        mochi_user_platform::process::find_by_name("package.service").map_err(|error| {
            format!(
                "Package service unavailable (errno {}).",
                error.errno().unwrap_or(0)
            )
        })?;
    if service == 0 {
        return Err(String::from("Package service is unavailable."));
    }
    let request_id = next_request_id(PACKAGE_REQUEST_PREFIX);
    let mut request = [0u8; package_protocol::HEADER_LEN];
    let request_length =
        package_protocol::encode_request(request_id, package_protocol::Request::List, &mut request)
            .map_err(|_| String::from("Unable to encode the package query."))?;
    let mut reply = vec![0u8; package_protocol::MAX_MESSAGE_LEN];
    let message = mochi_user_platform::ipc::call(service, &request[..request_length], &mut reply)
        .map_err(|error| {
        format!(
            "Unable to read installed packages (errno {}).",
            error.errno().unwrap_or(0)
        )
    })?;
    let reply_length = (message & 0xffff_ffff) as usize;
    let response = reply
        .get(..reply_length)
        .ok_or_else(|| String::from("Package service returned an invalid package list."))?;
    let records = match package_protocol::decode_list(response, request_id) {
        Ok(records) => records,
        Err(_) => {
            if let Ok(status) = package_protocol::decode_status(response, request_id) {
                return Err(format!(
                    "Unable to read installed packages (errno {}).",
                    status.unsigned_abs()
                ));
            }
            return Err(String::from(
                "Package service returned an invalid package list.",
            ));
        }
    };
    let mut packages = Vec::new();
    for record in records {
        let record = record
            .map_err(|_| String::from("Package service returned an invalid package record."))?;
        packages.push(InstalledPackage {
            package_id: record.package_id.to_string(),
            name: record.name.to_string(),
            version: record.version.to_string(),
            kind: record.kind.to_string(),
            built_in: record.flags & package_protocol::PACKAGE_FLAG_BUILT_IN != 0,
            removable: record.flags & package_protocol::PACKAGE_FLAG_REMOVABLE != 0,
        });
    }
    packages.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.package_id.cmp(&right.package_id))
    });
    Ok(packages)
}

#[cfg(not(target_os = "mochios"))]
fn query_installed_packages() -> Result<Vec<InstalledPackage>, String> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "mochios"))]
fn package_service_call(_request: package_protocol::Request<'_>) -> Result<(), String> {
    Err(String::from(
        "Package management is available only when App Store is running on mochiOS.",
    ))
}

fn refresh_installed_packages(packages: &State<Vec<InstalledPackage>>) -> Result<(), String> {
    let refreshed = query_installed_packages()?;
    packages.set(refreshed);
    Ok(())
}

fn mutate_package(path: &str, mutation: PackageMutation) -> Result<(), String> {
    package_service_call(match mutation {
        PackageMutation::Install => package_protocol::Request::Install(path),
        PackageMutation::Update => package_protocol::Request::Update(path),
    })
}

fn remove_package(package_id: &str) -> Result<(), String> {
    package_service_call(package_protocol::Request::Remove(package_id))
}

fn start_install(
    app: &CatalogApp,
    release: &CatalogRelease,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
    installed_packages: &State<Vec<InstalledPackage>>,
    mutation: PackageMutation,
) {
    let safe_bundle_id: String = app
        .bundle_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let file_id = next_request_id(PACKAGE_REQUEST_PREFIX);
    let path = format!("/tmp/appstore-{safe_bundle_id}-{file_id}.mpkg");
    if let Err(error) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        install_status.set(InstallStatus::Failed {
            bundle_id: app.bundle_id.clone(),
            message: format!("Unable to create the package file: {error}"),
        });
        return;
    }
    let pending = PendingInstall {
        request_id: 0,
        release: release.clone(),
        path,
        offset: 0,
        hasher: Sha256::new(),
        mutation,
    };
    install_status.set(InstallStatus::Downloading {
        bundle_id: app.bundle_id.clone(),
    });
    request_package_chunk(pending, pending_install, install_status, installed_packages);
}

fn app_detail(
    app: &CatalogApp,
    icons: &[LoadedIcon],
    release_status: State<ReleaseStatus>,
    pending_install: State<Option<PendingInstall>>,
    install_status: State<InstallStatus>,
    installed_packages: State<Vec<InstalledPackage>>,
    package_query_error: Option<String>,
    ui: &Rc<StoreUi>,
) -> VStack {
    let release = match release_status.get() {
        ReleaseStatus::Ready(release) if release.bundle_id == app.bundle_id => Some(release),
        _ => None,
    };
    let installed = installed_package(&installed_packages.get(), &app.bundle_id).cloned();
    let mutation = if installed.is_some() {
        PackageMutation::Update
    } else {
        PackageMutation::Install
    };
    let has_update = installed
        .as_ref()
        .zip(release.as_ref())
        .is_some_and(|(installed, release)| version_is_newer(&release.version, &installed.version));
    let (button_label, button_enabled) = match install_status.get() {
        InstallStatus::Downloading { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Downloading", false)
        }
        InstallStatus::Installing { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Installing", false)
        }
        InstallStatus::Removing { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Removing", false)
        }
        InstallStatus::Downloading { .. }
        | InstallStatus::Installing { .. }
        | InstallStatus::Removing { .. } => ("Busy", false),
        InstallStatus::Installed { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Installed", false)
        }
        _ if package_query_error.is_some() => ("Unavailable", false),
        _ if has_update => ("Update", true),
        _ if installed.is_some() => ("Installed", false),
        _ if release.is_some() => ("Get", true),
        _ => ("Get", false),
    };
    let version = release
        .as_ref()
        .map_or_else(|| app.version.clone(), |r| r.version.clone());
    let size = release
        .as_ref()
        .map(|r| format!("{:.1} MiB", r.size as f64 / 1_048_576.0));
    let mut get_button = ui
        .button(format!("install:{}", app.bundle_id), button_label)
        .style(ButtonStyle::Accent)
        .size(ButtonSize::Medium)
        .enabled(button_enabled);
    if let Some(release) = release {
        let install_app = app.clone();
        let install_pending = pending_install.clone();
        let install_status = install_status.clone();
        let install_packages = installed_packages.clone();
        get_button = get_button.on_click(move || {
            start_install(
                &install_app,
                &release,
                &install_pending,
                &install_status,
                &install_packages,
                mutation,
            )
        });
    }
    let status_message = match install_status.get() {
        InstallStatus::Failed {
            ref bundle_id,
            ref message,
        } if bundle_id == &app.bundle_id => Some(message.clone()),
        InstallStatus::Downloading { ref bundle_id } if bundle_id == &app.bundle_id => {
            Some(String::from("Downloading and verifying the package…"))
        }
        InstallStatus::Installing { ref bundle_id } if bundle_id == &app.bundle_id => {
            Some(String::from("Installing the verified package…"))
        }
        InstallStatus::Installed { ref bundle_id } if bundle_id == &app.bundle_id => {
            Some(String::from("The application was installed."))
        }
        InstallStatus::Removed { ref bundle_id } if bundle_id == &app.bundle_id => {
            Some(String::from("The application was removed."))
        }
        _ if package_query_error.is_some() => package_query_error.clone(),
        _ => match release_status.get() {
            ReleaseStatus::Loading { ref bundle_id } if bundle_id == &app.bundle_id => {
                Some(String::from("Checking compatibility…"))
            }
            ReleaseStatus::Unavailable { ref bundle_id } if bundle_id == &app.bundle_id => {
                Some(String::from(
                    "No x86_64 release compatible with this version of mochiOS is available.",
                ))
            }
            ReleaseStatus::Failed {
                ref bundle_id,
                ref message,
            } if bundle_id == &app.bundle_id => Some(message.clone()),
            _ => None,
        },
    };
    let mut actions = HStack::new()
        .alignment(StackAlignment::Center)
        .gap(StackGap::Small)
        .child(get_button);
    if let Some(installed) = installed.filter(|package| package.removable) {
        let remove_status = install_status.clone();
        let remove_packages = installed_packages.clone();
        let bundle_id = app.bundle_id.clone();
        actions = actions.child(
            ui.button(format!("remove:{}", app.bundle_id), "Remove")
                .style(ButtonStyle::Standard)
                .size(ButtonSize::Medium)
                .enabled(pending_install.get().is_none())
                .on_click(move || {
                    remove_status.set(InstallStatus::Removing {
                        bundle_id: bundle_id.clone(),
                    });
                    match remove_package(&installed.package_id)
                        .and_then(|()| refresh_installed_packages(&remove_packages))
                    {
                        Ok(()) => remove_status.set(InstallStatus::Removed {
                            bundle_id: bundle_id.clone(),
                        }),
                        Err(message) => remove_status.set(InstallStatus::Failed {
                            bundle_id: bundle_id.clone(),
                            message,
                        }),
                    }
                }),
        );
    }
    let mut information = SettingsSection::new("Information")
        .grouped()
        .row(settings_value("Version", "", version).compact())
        .row(settings_value("Developer", "", app.developer.clone()).compact());
    if !app.category.trim().is_empty() {
        information =
            information.row(settings_value("Category", "", app.category.clone()).compact());
    }
    if let Some(size) = size {
        information = information.row(settings_value("Download Size", "", size).compact());
    }
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::ExtraLarge)
        .child(
            HStack::new()
                .alignment(StackAlignment::Center)
                .gap(StackGap::Large)
                .child(app_icon(app, icons, 96.0))
                .child(
                    VStack::new()
                        .alignment(StackAlignment::Stretch)
                        .gap(StackGap::Small)
                        .child(Text::styled(app.name.clone(), TextRole::TitleMedium))
                        .child(
                            Text::caption(
                                app.subtitle
                                    .as_deref()
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or(&app.developer),
                            )
                            .tone(TextTone::Secondary),
                        )
                        .child(actions)
                        .layout()
                        .flex_grow(1.0)
                        .flex_shrink(1.0),
                ),
        );
    if let Some(message) = status_message {
        let failed = matches!(install_status.get(), InstallStatus::Failed { ref bundle_id, .. } if bundle_id == &app.bundle_id)
            || matches!(release_status.get(), ReleaseStatus::Failed { ref bundle_id, .. } if bundle_id == &app.bundle_id);
        content = content.child(Text::caption(message).color(if failed {
            Theme::current().colors.destructive
        } else {
            Theme::current().colors.text_secondary
        }));
    }
    if let Some(pending) = pending_install
        .get()
        .filter(|p| p.release.bundle_id == app.bundle_id)
    {
        let percent = if pending.release.size > 0 {
            pending.offset as f64 * 100.0 / pending.release.size as f64
        } else {
            0.0
        };
        content = content
            .child(Text::caption(format!("Downloaded {percent:.0}%")).tone(TextTone::Secondary));
    }
    content
        .child(Divider::new())
        .child(
            VStack::new()
                .alignment(StackAlignment::Stretch)
                .gap(StackGap::Small)
                .child(Text::styled("About", TextRole::TitleSmall))
                .child(Text::body(if app.description.trim().is_empty() {
                    "No description is available."
                } else {
                    &app.description
                })),
        )
        .child(information)
}

fn library_content(
    storefront: &Storefront,
    selection: &str,
    icons: &[LoadedIcon],
    installed_packages: &[InstalledPackage],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    package_query_error: Option<&str>,
    ui: &Rc<StoreUi>,
) -> VStack {
    let updates_only = selection == SELECTION_UPDATES;
    let title = if updates_only { "Updates" } else { "Installed" };
    let mut seen = HashSet::new();
    let apps: Vec<&CatalogApp> = storefront
        .sections
        .iter()
        .flat_map(|section| section.apps.iter())
        .filter(|app| {
            seen.insert(app.bundle_id.as_str())
                && installed_package(installed_packages, &app.bundle_id).is_some_and(|installed| {
                    !updates_only || version_is_newer(&app.version, &installed.version)
                })
        })
        .collect();
    let catalog_ids: HashSet<&str> = storefront
        .sections
        .iter()
        .flat_map(|section| section.apps.iter())
        .map(|app| app.bundle_id.as_str())
        .collect();
    let other_packages: Vec<_> = installed_packages
        .iter()
        .filter(|package| {
            !updates_only
                && package.kind == "application"
                && !catalog_ids.contains(package.package_id.as_str())
        })
        .collect();
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::DoubleExtraLarge)
        .child(Text::styled(title, TextRole::TitleMedium));
    if let Some(error) = package_query_error {
        return content
            .child(
                Text::body("Installed applications could not be loaded.").tone(TextTone::Secondary),
            )
            .child(Text::metadata(error));
    }
    if apps.is_empty() && other_packages.is_empty() {
        content = content.child(
            Text::body(if updates_only {
                "All applications are up to date."
            } else {
                "No applications are installed."
            })
            .tone(TextTone::Secondary),
        );
    } else if !apps.is_empty() {
        content = content.child(
            AdaptiveGrid::new(APP_TILE_WIDTH, APP_TILE_HEIGHT)
                .fill_columns()
                .spacing(
                    Theme::current().spacing.large,
                    Theme::current().spacing.small,
                )
                .children(apps.into_iter().map(|app| {
                    app_tile(
                        app,
                        icons,
                        selected_app.clone(),
                        release_status.clone(),
                        pending_release.clone(),
                        installed_packages,
                        ui,
                    )
                })),
        );
    }
    if !other_packages.is_empty() {
        content = content.child(
            SettingsSection::new("Other Installed Applications")
                .grouped()
                .rows(other_packages.into_iter().map(|package| {
                    settings_value(
                        package.name.clone(),
                        if package.built_in {
                            "Included with mochiOS"
                        } else {
                            "Not listed in this catalog"
                        },
                        package.version.clone(),
                    )
                    .compact()
                })),
        );
    }
    content
}

fn loading_content() -> VStack {
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(Text::styled("App Store", TextRole::TitleMedium).weight(600))
        .child(Text::body("Loading applications…").tone(TextTone::Secondary))
}

fn error_content(error: &str) -> VStack {
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(Text::styled("App Store is unavailable", TextRole::TitleMedium).weight(600))
        .child(
            Text::body(
                "The catalog could not be loaded. Check the network connection and reopen App Store.",
            )
            .tone(TextTone::Secondary),
        )
        .child(Text::metadata(error))
}

fn detail(
    catalog: &CatalogStatus,
    search: State<String>,
    selection: State<String>,
    icons: State<Vec<LoadedIcon>>,
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    pending_install: State<Option<PendingInstall>>,
    install_status: State<InstallStatus>,
    installed_packages: State<Vec<InstalledPackage>>,
    package_query_error: State<Option<String>>,
    ui: &Rc<StoreUi>,
) -> VStack {
    let is_detail = selected_app.get().is_some();
    let back_selection = selected_app.clone();
    let content = match catalog {
        CatalogStatus::Ready(storefront) if selected_app.get().is_some() => {
            let selected = selected_app.get().unwrap_or_default();
            match catalog_app(storefront, &selected) {
                Some(app) => app_detail(
                    app,
                    &icons.get(),
                    release_status.clone(),
                    pending_install.clone(),
                    install_status.clone(),
                    installed_packages.clone(),
                    package_query_error.get(),
                    ui,
                ),
                None => catalog_content(
                    storefront,
                    &search.get(),
                    &selection.get(),
                    &icons.get(),
                    selected_app.clone(),
                    release_status.clone(),
                    pending_release.clone(),
                    &installed_packages.get(),
                    ui,
                ),
            }
        }
        CatalogStatus::Ready(storefront)
            if selection.get().starts_with("library:") && search.get().trim().is_empty() =>
        {
            library_content(
                storefront,
                &selection.get(),
                &icons.get(),
                &installed_packages.get(),
                selected_app.clone(),
                release_status.clone(),
                pending_release.clone(),
                package_query_error.get().as_deref(),
                ui,
            )
        }
        CatalogStatus::Ready(storefront) => catalog_content(
            storefront,
            &search.get(),
            &selection.get(),
            &icons.get(),
            selected_app,
            release_status,
            pending_release,
            &installed_packages.get(),
            ui,
        ),
        CatalogStatus::Loading => loading_content(),
        CatalogStatus::Failed(error) => error_content(error),
    };

    let surface = Background::new()
        .background(Rectangle::new().color(RectangleColor::Surface))
        .content(
            Padding::only(
                Theme::current().spacing.large,
                Theme::current().spacing.large,
                Theme::current().spacing.extra_large,
                Theme::current().spacing.large,
            )
            .content(content),
        );

    let mut page = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::None);
    if is_detail {
        page = page.child(
            Padding::symmetric(
                Theme::current().spacing.large,
                Theme::current().spacing.small,
            )
            .content(
                HStack::new().child(
                    ui.button("back", "Back")
                        .style(ButtonStyle::Ghost)
                        .size(ButtonSize::Small)
                        .content(
                            HStack::new()
                                .alignment(StackAlignment::Center)
                                .gap(StackGap::Small)
                                .child(
                                    Icon::new(SymbolName::ChevronLeft)
                                        .size(Theme::current().layout.compact_icon_size),
                                )
                                .child(Text::label("Back")),
                        )
                        .on_click(move || back_selection.set(None)),
                ),
            )
            .layout()
            .flex_shrink(0.0),
        );
    }
    let scroll = if is_detail {
        ui.detail_scroll.clone()
    } else {
        ui.browse_scroll.clone()
    };
    page.child(
        Scroll::new(scroll)
            .axis(ScrollAxis::Vertical)
            .content(surface)
            .layout()
            .flex_grow(1.0)
            .flex_shrink(1.0),
    )
}

#[derive(Clone)]
struct LoadedIcon {
    bundle_id: String,
    image: ImageData,
}

fn decode_icon(bytes: &[u8]) -> Option<ImageData> {
    let image = ImageData::decode(bytes).ok()?;
    (image.width() <= MAX_ICON_DIMENSION && image.height() <= MAX_ICON_DIMENSION).then_some(image)
}

fn start_icon_requests(storefront: &Storefront) -> (Vec<LoadedIcon>, Vec<(u64, String)>) {
    let mut loaded = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::new();
    let mut request_index = 1u64;
    for section in &storefront.sections {
        for app in &section.apps {
            let Some(url) = app.icon.as_deref() else {
                continue;
            };
            if !seen.insert(app.bundle_id.as_str()) {
                continue;
            }
            let request_id = 0x4943_4f4e_0000_0000u64 | request_index;
            request_index = request_index.saturating_add(1);
            match api::start_icon_request(request_id, url) {
                Ok(DataRequest::Pending) => pending.push((request_id, app.bundle_id.clone())),
                Ok(DataRequest::Ready(bytes)) => {
                    if let Some(image) = decode_icon(&bytes) {
                        loaded.push(LoadedIcon {
                            bundle_id: app.bundle_id.clone(),
                            image,
                        });
                    }
                }
                Err(_) => {}
            }
        }
    }
    (loaded, pending)
}

struct AppStoreApp {
    ui: Rc<StoreUi>,
    catalog: CatalogStatus,
    catalog_revision: State<u64>,
    pending_catalog: Option<u64>,
    search: State<String>,
    selection: State<String>,
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    install_status: State<InstallStatus>,
    pending_release: State<Option<PendingRelease>>,
    pending_install: State<Option<PendingInstall>>,
    icons: State<Vec<LoadedIcon>>,
    pending_icons: Vec<(u64, String)>,
    installed_packages: State<Vec<InstalledPackage>>,
    package_query_error: State<Option<String>>,
}

impl App for AppStoreApp {
    type Body = StoreView;

    fn new() -> Self {
        let (installed_packages, package_query_error) = match query_installed_packages() {
            Ok(packages) => (packages, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let request_id = next_request_id(STOREFRONT_REQUEST_PREFIX);
        let (catalog, pending_catalog, icons, pending_icons) =
            match api::start_storefront_request(request_id) {
                Ok(DataRequest::Pending) => (
                    CatalogStatus::Loading,
                    Some(request_id),
                    Vec::new(),
                    Vec::new(),
                ),
                Ok(DataRequest::Ready(storefront)) => {
                    let (icons, pending_icons) = start_icon_requests(&storefront);
                    (CatalogStatus::Ready(storefront), None, icons, pending_icons)
                }
                Err(error) => (
                    CatalogStatus::Failed(error.to_string()),
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            };
        Self {
            ui: Rc::new(StoreUi::default()),
            catalog,
            catalog_revision: State::new(0),
            pending_catalog,
            search: State::new(String::new()),
            selection: State::new(String::from(SELECTION_ALL)),
            selected_app: State::new(None),
            release_status: State::new(ReleaseStatus::Idle),
            install_status: State::new(InstallStatus::Idle),
            pending_release: State::new(None),
            pending_install: State::new(None),
            icons: State::new(icons),
            pending_icons,
            installed_packages: State::new(installed_packages),
            package_query_error: State::new(package_query_error),
        }
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("App Store")
            .size(1040.0, 700.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        let _ = self.catalog_revision.get();
        let storefront = match &self.catalog {
            CatalogStatus::Ready(storefront) => Some(storefront),
            CatalogStatus::Loading | CatalogStatus::Failed(_) => None,
        };
        NavigationSplitView::new(
            Scroll::new(self.ui.sidebar_scroll.clone())
                .axis(ScrollAxis::Vertical)
                .content(navigation(
                    storefront,
                    self.search.clone(),
                    self.selection.clone(),
                    self.selected_app.clone(),
                    &self.ui,
                )),
            detail(
                &self.catalog,
                self.search.clone(),
                self.selection.clone(),
                self.icons.clone(),
                self.selected_app.clone(),
                self.release_status.clone(),
                self.pending_release.clone(),
                self.pending_install.clone(),
                self.install_status.clone(),
                self.installed_packages.clone(),
                self.package_query_error.clone(),
                &self.ui,
            ),
        )
        .flexible_sidebar(200.0, 240.0, 280.0)
        .minimum_detail_width(420.0)
        .shows_divider(false)
    }

    fn handle_platform_message(&mut self, message: &[u8]) -> bool {
        let Some(request_id) = api::response_request_id(message) else {
            return false;
        };
        if self.pending_catalog == Some(request_id) {
            let Some((_, result)) = api::finish_storefront_request(message) else {
                return false;
            };
            self.pending_catalog = None;
            match result {
                Ok(storefront) => {
                    let (icons, pending_icons) = start_icon_requests(&storefront);
                    self.catalog = CatalogStatus::Ready(storefront);
                    self.icons.set(icons);
                    self.pending_icons = pending_icons;
                }
                Err(error) => {
                    self.catalog = CatalogStatus::Failed(error.to_string());
                }
            }
            self.catalog_revision.update(|revision| {
                *revision = revision.wrapping_add(1);
            });
            return true;
        }
        if let Some(index) = self
            .pending_icons
            .iter()
            .position(|(pending_id, _)| *pending_id == request_id)
        {
            let Some((_, result)) = api::finish_icon_request(message) else {
                return false;
            };
            let (_, bundle_id) = self.pending_icons.swap_remove(index);
            if let Ok(bytes) = result
                && let Some(image) = decode_icon(&bytes)
            {
                self.icons.update(|icons| {
                    if let Some(icon) = icons.iter_mut().find(|icon| icon.bundle_id == bundle_id) {
                        icon.image = image;
                    } else {
                        icons.push(LoadedIcon { bundle_id, image });
                    }
                });
            }
            return true;
        }
        if let Some(pending) = self.pending_release.get()
            && pending.request_id == request_id
        {
            let Some((_, result)) = api::finish_release_request(message) else {
                return false;
            };
            self.pending_release.set(None);
            set_release_result(&pending.bundle_id, result, &self.release_status);
            return true;
        }
        if request_id & REQUEST_KIND_MASK == RELEASE_REQUEST_PREFIX {
            let _ = api::finish_release_request(message);
            return true;
        }
        if let Some(pending) = self.pending_install.get()
            && pending.request_id == request_id
        {
            let Some((_, result)) = api::finish_package_request(message) else {
                return false;
            };
            self.pending_install.set(None);
            match result {
                Ok(bytes) => consume_package_chunk(
                    pending,
                    bytes,
                    &self.pending_install,
                    &self.install_status,
                    &self.installed_packages,
                ),
                Err(error) => fail_install(
                    &pending,
                    error.to_string(),
                    &self.pending_install,
                    &self.install_status,
                ),
            }
            return true;
        }
        false
    }
}

fn main() -> Result<(), ViewKitError> {
    viewkit::run::<AppStoreApp>()
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogApp, ReleaseResponse, Storefront, app_matches_search, compatible_release,
        grouped_apps, package_digest_matches, version_is_newer,
    };

    #[test]
    fn search_from_a_detail_page_updates_results_and_survives_reconstruction() {
        use super::*;
        use viewkit::typography::TextMeasurer;
        let ui = Rc::new(StoreUi::default());
        let query = State::new(String::new());
        let selected = State::new(Some(String::from("org.example.app")));
        ui.search.set_focused(true);
        let make_search = || StoreSearch {
            field: TextField::with_interaction(ui.search.clone()),
            input: ui.search.clone(),
            query: query.clone(),
            selected_app: selected.clone(),
            scroll: ui.browse_scroll.clone(),
        };
        let theme = Theme::LIGHT;
        let mut text = TextMeasurer::new();
        let mut context = EventContext::new(&theme, &theme.typography, &mut text);
        let bounds = Rect::new(0.0, 0.0, 216.0, 30.0);
        make_search().handle_event(
            bounds,
            &ViewEvent::TextInput {
                text: String::from("note"),
            },
            &mut context,
        );
        assert_eq!(query.get(), "note");
        assert!(selected.get().is_none());
        make_search().handle_event(
            bounds,
            &ViewEvent::TextInput {
                text: String::from("s"),
            },
            &mut context,
        );
        assert_eq!(query.get(), "notes");
        assert!(ui.search.is_focused());
    }

    #[test]
    fn global_search_finds_apps_outside_the_selected_category() {
        use super::*;
        use viewkit::{
            draw_command::{DisplayList, DrawCommand},
            typography::TextMeasurer,
        };
        let storefront: Storefront = serde_json::from_str(r#"{"sections":[{"title":"Apps","apps":[
            {"bundle_id":"org.example.notes","name":"Notes","version":"1.0","developer":"mochiOS","category":"Productivity"},
            {"bundle_id":"org.example.paint","name":"Paint","version":"1.0","developer":"mochiOS","category":"Graphics"}
        ]}]}"#).unwrap();
        let ui = Rc::new(StoreUi::default());
        let content = catalog_content(
            &storefront,
            "Paint",
            "category:Productivity",
            &[],
            State::new(None),
            State::new(ReleaseStatus::Idle),
            State::new(None),
            &[],
            &ui,
        );
        let mut list = DisplayList::new();
        let theme = Theme::LIGHT;
        let mut text = TextMeasurer::new();
        content.paint(
            Rect::new(0.0, 0.0, 752.0, 500.0),
            &mut PaintContext::new(&mut list, &theme, &theme.typography, &mut text),
        );
        assert!(list.commands().iter().any(|cmd| matches!(cmd,
            DrawCommand::DrawText { command } if command.text == "Paint")));
    }

    #[test]
    fn update_versions_are_compared_numerically() {
        assert!(version_is_newer("1.10.0", "1.9.9"));
        assert!(version_is_newer("2.0", "1.99.99"));
        assert!(version_is_newer("1.0.0", "1.0.0-beta.1"));
        assert!(!version_is_newer("1.0", "1.0.0"));
        assert!(!version_is_newer("development", "1.0.0"));
    }

    #[test]
    fn catalog_search_matches_visible_app_details() {
        let app = CatalogApp {
            bundle_id: String::from("org.mochios.notes"),
            name: String::from("Notes"),
            version: String::from("1.0"),
            developer: String::from("mochiOS"),
            description: String::from("Write ideas"),
            subtitle: Some(String::from("Quick capture")),
            category: String::from("Productivity"),
            icon: None,
        };
        assert!(app_matches_search(&app, "notes"));
        assert!(app_matches_search(&app, "QUICK"));
        assert!(app_matches_search(&app, "write"));
        assert!(!app_matches_search(&app, "calendar"));
    }

    #[test]
    fn catalog_uses_api_category_and_deduplicates_apps() {
        let storefront: Storefront = serde_json::from_str(
            r#"{
                "categories": [{"name":"Development","slug":"development"}],
                "sections": [
                    {"title":"Apps","apps":[{
                        "bundle_id":"com.example.testapp",
                        "name":"TestApp",
                        "version":"0.1.0",
                        "developer":"tas0dev",
                        "description":"Test Application for mochiOS",
                        "category":"Development",
                        "icon":"https://github.com/mochiOS.png"
                    }]},
                    {"title":"Featured","apps":[{
                        "bundle_id":"com.example.testapp",
                        "name":"TestApp",
                        "version":"0.1.0",
                        "developer":"tas0dev",
                        "description":"Test Application for mochiOS",
                        "category":"Development"
                    }]}
                ]
            }"#,
        )
        .expect("storefront fixture");

        let groups = grouped_apps(&storefront, "", None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Development");
        assert_eq!(groups[0].1.len(), 1);
        assert_eq!(groups[0].1[0].name, "TestApp");
        assert_eq!(
            groups[0].1[0].icon.as_deref(),
            Some("https://github.com/mochiOS.png")
        );

        let development = grouped_apps(&storefront, "", Some("Development"));
        assert_eq!(development.len(), 1);
        assert_eq!(development[0].1.len(), 1);
        assert!(grouped_apps(&storefront, "", Some("Productivity")).is_empty());
    }

    #[test]
    fn release_selection_requires_exact_architecture_and_abi() {
        let valid: ReleaseResponse = serde_json::from_str(
            r#"{
                "bundle_id":"com.example.testapp",
                "releases":[{
                    "release_id":"release-1",
                    "bundle_id":"com.example.testapp",
                    "version":"1.0.0",
                    "size":5152,
                    "sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "architecture":"x86_64",
                    "abi":"mochios-1"
                }]
            }"#,
        )
        .expect("release fixture");
        assert!(
            compatible_release(valid, "com.example.testapp")
                .expect("valid release")
                .is_some()
        );

        let unknown: ReleaseResponse = serde_json::from_str(
            r#"{
                "bundle_id":"com.example.testapp",
                "releases":[{
                    "release_id":"legacy",
                    "bundle_id":"com.example.testapp",
                    "version":"0.1.0",
                    "size":5152,
                    "sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "architecture":null,
                    "abi":null
                }]
            }"#,
        )
        .expect("legacy fixture");
        assert!(compatible_release(unknown, "com.example.testapp").is_err());
    }

    #[test]
    fn downloaded_package_digest_is_checked() {
        assert!(package_digest_matches(
            b"mochiOS",
            "9f6f72aa042fc8677bdb3730c950ec73ae0c9bcbac22d286ba66b53623637a69"
        ));
        assert!(!package_digest_matches(
            b"tampered",
            "9f6f72aa042fc8677bdb3730c950ec73ae0c9bcbac22d286ba66b53623637a69"
        ));
        assert!(!package_digest_matches(b"mochiOS", "not-a-digest"));
    }
}
