#[cfg(target_os = "mochios")]
mod api;
#[cfg(not(target_os = "mochios"))]
#[path = "preview_api.rs"]
mod api;
mod catalog;

use api::{ApiError, DataRequest};
use catalog::{CatalogApp, CatalogRelease, ReleaseResponse, Storefront};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use viewkit::event::{EventContext, EventResult, ViewEvent};
use viewkit::platform::PointerButton;
use viewkit::prelude::*;
use viewkit::view::{Constraints, MeasureContext, PaintContext};

const LIBRARY: [&str; 2] = ["Updates", "Installed"];
const APP_ICON_RADIUS: f32 = 14.0;
const APP_TILE_WIDTH: f32 = 184.0;
const APP_TILE_HEIGHT: f32 = 158.0;
const APP_ICON_SIZE: f32 = 96.0;
const APP_BUTTON_WIDTH: f32 = 64.0;
const APP_BUTTON_HEIGHT: f32 = 28.0;
const MAX_ICON_DIMENSION: u32 = 2_048;
const SELECTION_ALL: &str = "all";
const SELECTION_UPDATES: &str = "library:updates";
const SELECTION_INSTALLED: &str = "library:installed";
const STOREFRONT_REQUEST_PREFIX: u64 = 0x5354_4f52_0000_0000;
const RELEASE_REQUEST_PREFIX: u64 = 0x5245_4c53_0000_0000;
const PACKAGE_REQUEST_PREFIX: u64 = 0x504b_4744_0000_0000;
const REQUEST_KIND_MASK: u64 = 0xffff_ffff_0000_0000;
const REQUEST_SEQUENCE_MASK: u64 = 0x0000_0000_ffff_ffff;
#[cfg(target_os = "mochios")]
const INSTALL_REQUEST_OPCODE: u32 = 0x494e_5354;

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

type StoreView = NavigationSplitView<VStack, VStack>;

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
    Installed { bundle_id: String },
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
        return Err(String::from("The release response did not match this application."));
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
) -> VStack {
    let all_selection = selection.clone();
    let all_selected_app = selected_app.clone();
    let mut browse = SidebarSection::new("Explore").item(
        SidebarItem::new("All Apps")
            .symbol(SymbolName::Grid)
            .selected(selection.get() == SELECTION_ALL)
            .on_select(move || {
                all_selection.set(String::from(SELECTION_ALL));
                all_selected_app.set(None);
            }),
    );
    if let Some(storefront) = storefront {
        for category in &storefront.categories {
            let target = category_selection(&category.name);
            let category_selection = selection.clone();
            let category_selected_app = selected_app.clone();
            browse = browse.item(
                SidebarItem::new(category.name.clone())
                    .symbol(SymbolName::Tag)
                    .selected(selection.get() == target)
                    .on_select(move || {
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
        library = library.item(
            SidebarItem::new(label)
                .symbol(symbol)
                .selected(selection.get() == target)
                .on_select(move || {
                    library_selection.set(String::from(target));
                    library_selected_app.set(None);
                }),
        );
    }

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Large)
        .child(
            TextField::new(search.binding())
                .placeholder("Search applications")
                .size(TextFieldSize::Small)
                .radius(CornerRadius::Custom(6.0))
                .leading_symbol(SymbolName::Search)
                .frame(
                    Theme::current().layout.compact_form_control_width,
                    Theme::current().layout.control_height,
                ),
        )
        .child(browse)
        .child(library)
        .child(Spacer::new())
}

struct AppTile {
    app: CatalogApp,
    icon: Option<ImageData>,
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
    pressed: Cell<bool>,
}

impl AppTile {
    fn new(
        app: &CatalogApp,
        icons: &[LoadedIcon],
        selected_app: State<Option<String>>,
        release_status: State<ReleaseStatus>,
        pending_release: State<Option<PendingRelease>>,
    ) -> Self {
        Self {
            app: app.clone(),
            icon: icons
                .iter()
                .find(|icon| icon.bundle_id == app.bundle_id)
                .map(|icon| icon.image.clone()),
            selected_app,
            release_status,
            pending_release,
            pressed: Cell::new(false),
        }
    }

    fn default_icon() -> Option<SvgData> {
        static ICON: OnceLock<Option<SvgData>> = OnceLock::new();
        ICON.get_or_init(|| {
            SvgData::decode(include_bytes!("../../binder/resources/appicon.svg")).ok()
        })
        .clone()
    }

    fn icon_bounds(bounds: Rect) -> Rect {
        Rect::new(
            bounds.origin.x + (bounds.size.width - APP_ICON_SIZE) * 0.5,
            bounds.origin.y,
            APP_ICON_SIZE,
            APP_ICON_SIZE,
        )
    }

    fn name_bounds(bounds: Rect) -> Rect {
        Rect::new(bounds.origin.x, bounds.origin.y + 96.0, bounds.size.width, 25.0)
    }

    fn button_bounds(bounds: Rect) -> Rect {
        Rect::new(
            bounds.origin.x + (bounds.size.width - APP_BUTTON_WIDTH) * 0.5,
            bounds.origin.y + 130.0,
            APP_BUTTON_WIDTH,
            APP_BUTTON_HEIGHT,
        )
    }
}

impl View for AppTile {
    fn measure(&self, constraints: Constraints, _context: &mut MeasureContext<'_>) -> Size {
        constraints.constrain(Size::new(APP_TILE_WIDTH, APP_TILE_HEIGHT))
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        let mut node = AccessibilityNode::new(AccessibilityRole::Button, bounds);
        node.label = Some(format!("Open {}", self.app.name));
        context.record_accessibility(node);
        if let Some(icon) = self.icon.clone() {
            Image::new(icon)
                .content_mode(ImageContentMode::Fill)
                .radius(CornerRadius::Custom(APP_ICON_RADIUS))
                .accessibility_label(format!("{} icon", self.app.name))
                .paint(Self::icon_bounds(bounds), context);
        } else if let Some(icon) = Self::default_icon() {
            Svg::new(icon)
                .content_mode(SvgContentMode::Fill)
                .radius(CornerRadius::Custom(APP_ICON_RADIUS))
                .accessibility_label(format!("{} icon", self.app.name))
                .paint(Self::icon_bounds(bounds), context);
        }
        Text::styled(self.app.name.clone(), TextRole::Label)
            .weight(500)
            .alignment(TextAlignment::Center)
            .paint(Self::name_bounds(bounds), context);
        Button::new("Get")
            .style(ButtonStyle::Accent)
            .size(ButtonSize::Small)
            .radius(CornerRadius::Small)
            .accessibility_label(format!("Get {}", self.app.name))
            .paint(Self::button_bounds(bounds), context);
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        match event {
            ViewEvent::PointerMoved { position } => {
                if bounds.contains(*position) {
                    context.set_cursor(CursorIcon::Pointer);
                    EventResult::Consumed
                } else if self.pressed.get() {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            ViewEvent::PointerPressed {
                position,
                button: PointerButton::Primary,
            } if bounds.contains(*position) => {
                self.pressed.set(true);
                EventResult::Consumed
            }
            ViewEvent::PointerReleased {
                position,
                button: PointerButton::Primary,
            } => {
                let was_pressed = self.pressed.replace(false);
                if was_pressed && bounds.contains(*position) {
                    open_app(
                        &self.app.bundle_id,
                        &self.selected_app,
                        &self.release_status,
                        &self.pending_release,
                    );
                    context.request_redraw();
                }
                if was_pressed {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            ViewEvent::PointerLeft => {
                self.pressed.set(false);
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }
}

fn app_matches_search(app: &CatalogApp, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty()
        || app.name.to_ascii_lowercase().contains(&query)
        || app.developer.to_ascii_lowercase().contains(&query)
        || app.description.to_ascii_lowercase().contains(&query)
        || app.subtitle.as_deref().unwrap_or("").to_ascii_lowercase().contains(&query)
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
    apps: Vec<&CatalogApp>,
    icons: &[LoadedIcon],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_release: State<Option<PendingRelease>>,
) -> VStack {
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::DoubleExtraLarge)
        .child(Text::styled(title, TextRole::TitleMedium).weight(600))
        .child(
            Padding::only(0.0, 0.0, 0.0, Theme::current().spacing.large).content(
                AdaptiveGrid::new(APP_TILE_WIDTH, APP_TILE_HEIGHT)
                    .spacing(
                        Theme::current().spacing.extra_large,
                        Theme::current().spacing.double_extra_large,
                    )
                    .children(
                        apps.into_iter()
                            .map(|app| {
                                AppTile::new(
                                    app,
                                    icons,
                                    selected_app.clone(),
                                    release_status.clone(),
                                    pending_release.clone(),
                                )
                            }),
                    ),
            ),
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
) -> VStack {
    let category = selection.strip_prefix("category:");
    let groups = grouped_apps(storefront, query, category);
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::TripleExtraLarge);
    for (title, apps) in groups.iter() {
        content = content.child(section_view(
            title.clone(),
            apps.clone(),
            icons,
            selected_app.clone(),
            release_status.clone(),
            pending_release.clone(),
        ));
    }

    if groups.is_empty() {
        content = content.child(
            Text::styled(
                if query.trim().is_empty() {
                    "No apps are currently available"
                } else {
                    "No matching apps"
                },
                TextRole::TitleMedium,
            )
            .weight(600),
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
    } else if let Some(icon) = AppTile::default_icon() {
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
    let result = install_package(&pending.path);
    let _ = fs::remove_file(&pending.path);
    match result {
        Ok(()) => install_status.set(InstallStatus::Installed {
            bundle_id: pending.release.bundle_id,
        }),
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
        finish_install(pending, pending_install, install_status);
    } else {
        request_package_chunk(pending, pending_install, install_status);
    }
}

fn request_package_chunk(
    mut pending: PendingInstall,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
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
            consume_package_chunk(pending, bytes, pending_install, install_status);
        }
        Err(error) => fail_install(
            &pending,
            error.to_string(),
            pending_install,
            install_status,
        ),
    }
}

#[cfg(target_os = "mochios")]
fn install_package(path: &str) -> Result<(), String> {
    let service = mochi_user_platform::process::find_by_name("package.service")
        .map_err(|error| format!("Package service unavailable (errno {}).", error.errno().unwrap_or(0)))?;
    if service == 0 {
        return Err(String::from("Package service is unavailable."));
    }
    let mut request = Vec::with_capacity(4 + path.len());
    request.extend_from_slice(&INSTALL_REQUEST_OPCODE.to_le_bytes());
    request.extend_from_slice(path.as_bytes());
    let mut reply = [0u8; 8];
    let message = mochi_user_platform::ipc::call(service, &request, &mut reply)
        .map_err(|error| format!("Installation failed (errno {}).", error.errno().unwrap_or(0)))?;
    if (message & 0xffff_ffff) as usize != reply.len() {
        return Err(String::from("Package service returned an invalid response."));
    }
    let status = u64::from_le_bytes(reply);
    if status == 0 {
        Ok(())
    } else {
        Err(format!("Installation failed (errno {status})."))
    }
}

#[cfg(not(target_os = "mochios"))]
fn install_package(_path: &str) -> Result<(), String> {
    Err(String::from(
        "Installation is available only when App Store is running on mochiOS.",
    ))
}

fn start_install(
    app: &CatalogApp,
    release: &CatalogRelease,
    pending_install: &State<Option<PendingInstall>>,
    install_status: &State<InstallStatus>,
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
    };
    install_status.set(InstallStatus::Downloading {
        bundle_id: app.bundle_id.clone(),
    });
    request_package_chunk(pending, pending_install, install_status);
}

fn app_detail(
    app: &CatalogApp,
    icons: &[LoadedIcon],
    selected_app: State<Option<String>>,
    release_status: State<ReleaseStatus>,
    pending_install: State<Option<PendingInstall>>,
    install_status: State<InstallStatus>,
) -> VStack {
    let back_selection = selected_app.clone();
    let release = match release_status.get() {
        ReleaseStatus::Ready(release) if release.bundle_id == app.bundle_id => Some(release),
        _ => None,
    };
    let (button_label, button_enabled) = match install_status.get() {
        InstallStatus::Downloading { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Downloading", false)
        }
        InstallStatus::Installing { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Installing", false)
        }
        InstallStatus::Downloading { .. } | InstallStatus::Installing { .. } => ("Busy", false),
        InstallStatus::Installed { ref bundle_id } if bundle_id == &app.bundle_id => {
            ("Installed", false)
        }
        _ if release.is_some() => ("Get", true),
        _ => ("Get", false),
    };
    let mut get_button = Button::new(button_label)
        .style(ButtonStyle::Accent)
        .size(ButtonSize::Small)
        .enabled(button_enabled);
    if let Some(release) = release {
        let install_app = app.clone();
        let install_pending = pending_install.clone();
        let install_status = install_status.clone();
        get_button = get_button.on_click(move || {
            start_install(
                &install_app,
                &release,
                &install_pending,
                &install_status,
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
        _ => match release_status.get() {
            ReleaseStatus::Loading { ref bundle_id } if bundle_id == &app.bundle_id => {
                Some(String::from("Checking compatibility…"))
            }
            ReleaseStatus::Unavailable { ref bundle_id } if bundle_id == &app.bundle_id => Some(
                String::from("No x86_64 release compatible with this version of mochiOS is available."),
            ),
            ReleaseStatus::Failed {
                ref bundle_id,
                ref message,
            } if bundle_id == &app.bundle_id => Some(message.clone()),
            _ => None,
        },
    };
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::DoubleExtraLarge)
        .child(
            Button::new("Back")
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .alignment(ZStackAlignment::Leading)
                .on_click(move || back_selection.set(None))
                .layout()
                .width(72.0),
        )
        .child(
            HStack::new()
                .alignment(StackAlignment::Center)
                .gap(StackGap::ExtraLarge)
                .child(app_icon(app, icons, 112.0))
                .child(
                    VStack::new()
                        .alignment(StackAlignment::Start)
                        .gap(StackGap::ExtraSmall)
                        .child(Text::styled(app.name.clone(), TextRole::TitleLarge).weight(600))
                        .child(Text::body(app.developer.clone()).tone(TextTone::Secondary))
                        .child(get_button),
                ),
        )
        .child(Text::body(app.description.clone()))
        .child(Text::metadata(app.category.clone()));
    if let Some(message) = status_message {
        content = content.child(Text::body(message).tone(TextTone::Secondary));
    }
    content
}

fn library_content(selection: &str) -> VStack {
    let (title, message) = if selection == SELECTION_UPDATES {
        (
            "Updates",
            "Application updates will appear here when package installation is available.",
        )
    } else {
        (
            "Installed",
            "Installed applications will appear here when package installation is available.",
        )
    };
    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::Small)
        .child(Text::styled(title, TextRole::TitleMedium).weight(600))
        .child(Text::body(message).tone(TextTone::Secondary))
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
) -> VStack {
    let content = match catalog {
        CatalogStatus::Ready(storefront) if selected_app.get().is_some() => {
            let selected = selected_app.get().unwrap_or_default();
            match catalog_app(storefront, &selected) {
                Some(app) => app_detail(
                    app,
                    &icons.get(),
                    selected_app.clone(),
                    release_status.clone(),
                    pending_install.clone(),
                    install_status.clone(),
                ),
                None => {
                    catalog_content(
                        storefront,
                        &search.get(),
                        &selection.get(),
                        &icons.get(),
                        selected_app.clone(),
                        release_status.clone(),
                        pending_release.clone(),
                    )
                }
            }
        }
        CatalogStatus::Ready(_) if selection.get().starts_with("library:") => {
            library_content(&selection.get())
        }
        CatalogStatus::Ready(storefront) => catalog_content(
            storefront,
            &search.get(),
            &selection.get(),
            &icons.get(),
            selected_app,
            release_status,
            pending_release,
        ),
        CatalogStatus::Loading => loading_content(),
        CatalogStatus::Failed(error) => error_content(error),
    };

    let surface = Background::new()
        .background(Rectangle::new().color(RectangleColor::Surface))
        .content(
            Padding::only(
                Theme::current().spacing.medium,
                Theme::current().spacing.medium,
                Theme::current().spacing.extra_large,
                Theme::current().spacing.medium,
            )
            .content(content),
        );

    VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::None)
        .child(Scroll::vertical(surface).layout().flex_grow(1.0))
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
}

impl App for AppStoreApp {
    type Body = StoreView;

    fn new() -> Self {
        let request_id = next_request_id(STOREFRONT_REQUEST_PREFIX);
        let (catalog, pending_catalog, icons, pending_icons) =
            match api::start_storefront_request(request_id) {
                Ok(DataRequest::Pending) => {
                    (CatalogStatus::Loading, Some(request_id), Vec::new(), Vec::new())
                }
                Ok(DataRequest::Ready(storefront)) => {
                    let (icons, pending_icons) = start_icon_requests(&storefront);
                    (
                        CatalogStatus::Ready(storefront),
                        None,
                        icons,
                        pending_icons,
                    )
                }
                Err(error) => (
                    CatalogStatus::Failed(error.to_string()),
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            };
        Self {
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
            navigation(
                storefront,
                self.search.clone(),
                self.selection.clone(),
                self.selected_app.clone(),
            ),
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
            ),
        )
        .flexible_sidebar(200.0, 240.0, 280.0)
        .minimum_detail_width(520.0)
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
        grouped_apps, package_digest_matches,
    };

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
