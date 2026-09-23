#[cfg(target_os = "mochios")]
mod api;
#[cfg(not(target_os = "mochios"))]
#[path = "preview_api.rs"]
mod api;
mod catalog;

use api::{ApiError, IconRequest};
use catalog::{CatalogApp, Storefront};
use std::collections::HashSet;
use std::sync::OnceLock;
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

type StoreView = NavigationLayout;

fn category_selection(name: &str) -> String {
    format!("category:{name}")
}

fn navigation(
    storefront: Option<&Storefront>,
    search: State<String>,
    selection: State<String>,
) -> StackChild {
    let all_selection = selection.clone();
    let mut browse = SidebarSection::new("Explore").item(
        SidebarItem::new("All Apps")
            .symbol(SymbolName::Grid)
            .selected(selection.get() == SELECTION_ALL)
            .on_select(move || all_selection.set(String::from(SELECTION_ALL))),
    );
    if let Some(storefront) = storefront {
        for category in &storefront.categories {
            let target = category_selection(&category.name);
            let category_selection = selection.clone();
            browse = browse.item(
                SidebarItem::new(category.name.clone())
                    .symbol(SymbolName::Tag)
                    .selected(selection.get() == target)
                    .on_select(move || category_selection.set(target.clone())),
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
        library = library.item(
            SidebarItem::new(label)
                .symbol(symbol)
                .selected(selection.get() == target)
                .on_select(move || library_selection.set(String::from(target))),
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
        .into_stack_child()
}

struct AppTile {
    app: CatalogApp,
    icon: Option<ImageData>,
}

impl AppTile {
    fn new(app: &CatalogApp, icons: &[LoadedIcon]) -> Self {
        Self {
            app: app.clone(),
            icon: icons
                .iter()
                .find(|icon| icon.bundle_id == app.bundle_id)
                .map(|icon| icon.image.clone()),
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
            .enabled(false)
            .accessibility_label(format!("Get {}", self.app.name))
            .paint(Self::button_bounds(bounds), context);
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

fn section_view(title: String, apps: Vec<&CatalogApp>, icons: &[LoadedIcon]) -> VStack {
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
                    .children(apps.into_iter().map(|app| AppTile::new(app, icons))),
            ),
        )
}

fn catalog_content(
    storefront: &Storefront,
    query: &str,
    selection: &str,
    icons: &[LoadedIcon],
) -> VStack {
    let category = selection.strip_prefix("category:");
    let groups = grouped_apps(storefront, query, category);
    let mut content = VStack::new()
        .alignment(StackAlignment::Stretch)
        .gap(StackGap::TripleExtraLarge);
    for (title, apps) in groups.iter() {
        content = content.child(section_view(title.clone(), apps.clone(), icons));
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

fn error_content(error: &ApiError) -> VStack {
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
        .child(Text::metadata(error.to_string()))
}

fn detail(
    catalog: &Result<Storefront, ApiError>,
    search: State<String>,
    selection: State<String>,
    icons: State<Vec<LoadedIcon>>,
) -> VStack {
    let content = match catalog {
        Ok(_) if selection.get().starts_with("library:") => library_content(&selection.get()),
        Ok(storefront) => catalog_content(
            storefront,
            &search.get(),
            &selection.get(),
            &icons.get(),
        ),
        Err(error) => error_content(error),
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

fn start_icon_requests(
    catalog: &Result<Storefront, ApiError>,
) -> (Vec<LoadedIcon>, Vec<(u64, String)>) {
    let mut loaded = Vec::new();
    let mut pending = Vec::new();
    let Ok(storefront) = catalog else {
        return (loaded, pending);
    };
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
                Ok(IconRequest::Pending) => pending.push((request_id, app.bundle_id.clone())),
                Ok(IconRequest::Ready(bytes)) => {
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
    catalog: Result<Storefront, ApiError>,
    search: State<String>,
    selection: State<String>,
    icons: State<Vec<LoadedIcon>>,
    pending_icons: Vec<(u64, String)>,
}

impl App for AppStoreApp {
    type Body = StoreView;

    fn new() -> Self {
        let catalog = api::fetch_storefront();
        let (icons, pending_icons) = start_icon_requests(&catalog);
        Self {
            catalog,
            search: State::new(String::new()),
            selection: State::new(String::from(SELECTION_ALL)),
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
        let storefront = self.catalog.as_ref().ok();
        NavigationLayout::new(
            HStack::new().child(Spacer::new()),
            navigation(
                storefront,
                self.search.clone(),
                self.selection.clone(),
            ),
            detail(
                &self.catalog,
                self.search.clone(),
                self.selection.clone(),
                self.icons.clone(),
            ),
        )
    }

    fn handle_platform_message(&mut self, message: &[u8]) -> bool {
        let Some((request_id, result)) = api::finish_icon_request(message) else {
            return false;
        };
        let Some(index) = self
            .pending_icons
            .iter()
            .position(|(pending_id, _)| *pending_id == request_id)
        else {
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
        true
    }
}

fn main() -> Result<(), ViewKitError> {
    viewkit::run::<AppStoreApp>()
}

#[cfg(test)]
mod tests {
    use super::{CatalogApp, Storefront, app_matches_search, grouped_apps};

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
}
