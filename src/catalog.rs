use serde::Deserialize;

pub(crate) const PRODUCTION_API_BASE_URL: &str = "https://api.store.mochios.org/v1";

#[derive(Debug, Deserialize)]
pub(crate) struct Storefront {
    #[serde(default)]
    pub sections: Vec<StorefrontSection>,
    #[serde(default)]
    pub categories: Vec<StorefrontCategory>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StorefrontSection {
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub apps: Vec<CatalogApp>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StorefrontCategory {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct CatalogApp {
    pub bundle_id: String,
    pub name: String,
    pub version: String,
    pub developer: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
}
