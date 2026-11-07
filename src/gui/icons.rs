// spot's icons, drawn for a 24 point square and colored by whatever draws them.

use rune::Icon;

const fn i(name: &'static str, body: &'static str) -> Icon {
    Icon { name, view: 24.0, body }
}

pub static PLAY: Icon = i("play", r#"<path d="M8.2 5.2C8.2 4.5 9 4.1 9.6 4.5L19 11.2C19.6 11.6 19.6 12.4 19 12.8L9.6 19.5C9 19.9 8.2 19.5 8.2 18.8Z" fill="currentColor"/>"#);
pub static PAUSE: Icon = i("pause", r#"<rect x="6.5" y="4.5" width="4" height="15" rx="1.3" fill="currentColor"/><rect x="13.5" y="4.5" width="4" height="15" rx="1.3" fill="currentColor"/>"#);
pub static NEXT: Icon = i("next", r#"<path d="M5.5 6.2C5.5 5.5 6.3 5.1 6.9 5.5L14.6 11.2C15.1 11.6 15.1 12.4 14.6 12.8L6.9 18.5C6.3 18.9 5.5 18.5 5.5 17.8Z" fill="currentColor"/><rect x="16.2" y="5.2" width="2.4" height="13.6" rx="1.2" fill="currentColor"/>"#);
pub static PREV: Icon = i("prev", r#"<path d="M18.5 6.2C18.5 5.5 17.7 5.1 17.1 5.5L9.4 11.2C8.9 11.6 8.9 12.4 9.4 12.8L17.1 18.5C17.7 18.9 18.5 18.5 18.5 17.8Z" fill="currentColor"/><rect x="5.4" y="5.2" width="2.4" height="13.6" rx="1.2" fill="currentColor"/>"#);
pub static SHUFFLE: Icon = i("shuffle", r#"<path d="M3.5 7H6.6C8.2 7 9.4 7.8 10.3 9.2L13.7 14.8C14.6 16.2 15.8 17 17.4 17H20M17.5 14.5L20 17L17.5 19.5M3.5 17H6.6C7.6 17 8.5 16.7 9.2 16.1M20 7H17.4C16.4 7 15.5 7.3 14.8 7.9M17.5 4.5L20 7L17.5 9.5" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#);
pub static REPEAT: Icon = i("repeat", r#"<path d="M4.5 11.5V10C4.5 8 6 6.5 8 6.5H19M16.5 4L19 6.5L16.5 9M19.5 12.5V14C19.5 16 18 17.5 16 17.5H5M7.5 20L5 17.5L7.5 15" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#);
pub static SEARCH: Icon = i("search", r#"<circle cx="10.5" cy="10.5" r="6" stroke="currentColor" stroke-width="1.9" fill="none"/><path d="M15.2 15.2L20 20" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"/>"#);
pub static FIND: Icon = i("find", r#"<path d="M4 6.5H14M4 11.5H11M4 16.5H9" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><circle cx="16.5" cy="14.5" r="3.2" stroke="currentColor" stroke-width="1.8" fill="none"/><path d="M18.8 16.8L21 19" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#);
pub static BACK: Icon = i("back", r#"<path d="M14.5 5.5L8 12L14.5 18.5" stroke="currentColor" stroke-width="2.1" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#);
pub static HEART: Icon = i("heart", r#"<path d="M12 20C12 20 4 15.3 4 9.6C4 7 6 5 8.5 5C10 5 11.2 5.7 12 6.8C12.8 5.7 14 5 15.5 5C18 5 20 7 20 9.6C20 15.3 12 20 12 20Z" fill="currentColor"/>"#);
pub static NOTE: Icon = i("note", r#"<path d="M9.5 17V6.2L18.5 4.5V15" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/><circle cx="7.2" cy="17.2" r="2.4" fill="currentColor"/><circle cx="16.2" cy="15.2" r="2.4" fill="currentColor"/>"#);
pub static MIC: Icon = i("mic", r#"<rect x="9" y="3.5" width="6" height="11" rx="3" fill="currentColor"/><path d="M5.8 11.5C5.8 15 8.6 17.6 12 17.6C15.4 17.6 18.2 15 18.2 11.5M12 17.6V20.5" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round"/>"#);
pub static VOLUME: Icon = i("volume", r#"<path d="M4 9.3H7L11.2 5.6C11.7 5.2 12.4 5.5 12.4 6.2V17.8C12.4 18.5 11.7 18.8 11.2 18.4L7 14.7H4C3.6 14.7 3.3 14.4 3.3 14V10C3.3 9.6 3.6 9.3 4 9.3Z" fill="currentColor"/><path d="M15.5 9C16.3 9.8 16.8 10.8 16.8 12C16.8 13.2 16.3 14.2 15.5 15M18 6.5C19.5 7.9 20.4 9.8 20.4 12C20.4 14.2 19.5 16.1 18 17.5" stroke="currentColor" stroke-width="1.7" fill="none" stroke-linecap="round"/>"#);
pub static QUEUE: Icon = i("queue", r#"<path d="M4 6.5H16M4 11.5H16M4 16.5H11" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><path d="M15 14.5V20L19.5 17.3Z" fill="currentColor"/>"#);
pub static ALBUM: Icon = i("album", r#"<circle cx="12" cy="12" r="8" stroke="currentColor" stroke-width="1.8" fill="none"/><circle cx="12" cy="12" r="2.2" fill="currentColor"/>"#);
pub static PERSON: Icon = i("person", r#"<circle cx="12" cy="8.5" r="3.8" fill="currentColor"/><path d="M4.8 20C5.6 16.4 8.5 14.3 12 14.3C15.5 14.3 18.4 16.4 19.2 20" stroke="currentColor" stroke-width="1.9" fill="none" stroke-linecap="round"/>"#);
pub static PLUS: Icon = i("plus", r#"<path d="M12 5.5V18.5M5.5 12H18.5" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#);
pub static MORE: Icon = i("more", r#"<circle cx="5.5" cy="12" r="1.8" fill="currentColor"/><circle cx="12" cy="12" r="1.8" fill="currentColor"/><circle cx="18.5" cy="12" r="1.8" fill="currentColor"/>"#);
pub static MUTED: Icon = i("muted", r#"<path d="M4 9.3H7L11.2 5.6C11.7 5.2 12.4 5.5 12.4 6.2V17.8C12.4 18.5 11.7 18.8 11.2 18.4L7 14.7H4C3.6 14.7 3.3 14.4 3.3 14V10C3.3 9.6 3.6 9.3 4 9.3Z" fill="currentColor"/><path d="M15.8 9.5L20.5 14.5M20.5 9.5L15.8 14.5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#);
pub static KEYS: Icon = i("keys", r#"<rect x="3" y="6" width="18" height="12" rx="2.5" stroke="currentColor" stroke-width="1.7" fill="none"/><path d="M7 10H7.01M10.3 10H10.31M13.7 10H13.71M17 10H17.01M8 14H16" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"/>"#);
pub static SPARK: Icon = i("spark", r#"<path d="M12 3.5L13.7 9.6C13.9 10.2 14.3 10.6 14.9 10.8L20.5 12L14.9 13.2C14.3 13.4 13.9 13.8 13.7 14.4L12 20.5L10.3 14.4C10.1 13.8 9.7 13.4 9.1 13.2L3.5 12L9.1 10.8C9.7 10.6 10.1 10.2 10.3 9.6Z" fill="currentColor"/>"#);
pub static PIN: Icon = i("pin", r#"<path d="M9 4.5H15L14 10L17 13.2V14.5H7V13.2L10 10Z" fill="currentColor" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/><path d="M12 14.5V20" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#);
pub static GEAR: Icon = i("gear", r#"<circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="1.8" fill="none"/><path d="M12 3.5V5.6M12 18.4V20.5M20.5 12H18.4M5.6 12H3.5M18 6L16.5 7.5M7.5 16.5L6 18M18 18L16.5 16.5M7.5 7.5L6 6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#);
