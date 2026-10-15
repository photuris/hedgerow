//! UI components. Shared state lives in [`crate::app::Ui`].

mod chat;
mod composer;
mod confirm;
mod contacts;
mod icons;
mod search;
mod settings;
mod sidebar;
mod toast;

pub use chat::Chat;
pub use contacts::{Contacts, ExportDialog};
pub use icons::Icon;
pub use search::SearchResults;
pub use settings::Settings;
pub use sidebar::Sidebar;
pub use toast::Toast;
