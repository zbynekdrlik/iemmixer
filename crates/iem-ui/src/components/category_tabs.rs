//! Category tabs component

use leptos::prelude::*;

/// Category type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Main,
    Mics,
    Stems,
    Tech,
    Mixes,
    Hidden,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::Main => "Main",
            Category::Mics => "Mics",
            Category::Stems => "Stems",
            Category::Tech => "Tech",
            Category::Mixes => "Mixes",
            Category::Hidden => "Hidden",
        }
    }

    pub fn matches(&self, category: &str) -> bool {
        match self {
            Category::Main => false,   // Main has special rendering
            Category::Hidden => false, // Hidden has special rendering
            Category::Mics => category == "mics",
            Category::Stems => category == "stems",
            Category::Tech => category == "tech",
            Category::Mixes => category == "mixes",
        }
    }

    pub fn class(&self) -> &'static str {
        match self {
            Category::Main => "main",
            Category::Mics => "mics",
            Category::Stems => "stems",
            Category::Tech => "tech",
            Category::Mixes => "mixes",
            Category::Hidden => "hidden",
        }
    }
}

/// Category tabs component
#[component]
pub fn CategoryTabs(
    /// Currently selected category
    active: ReadSignal<Category>,
    /// Called when category changes
    on_select: impl Fn(Category) + 'static + Clone,
    /// Whether to show the Hidden tab (only when there are hidden channels)
    #[prop(default = false.into())]
    show_hidden: Signal<bool>,
    /// Whether to show the Mixes tab (only when mix channels exist, engineer only)
    #[prop(default = false.into())]
    show_mixes: Signal<bool>,
) -> impl IntoView {
    let base_categories = [
        Category::Main,
        Category::Mics,
        Category::Stems,
        Category::Tech,
    ];

    let on_select_mixes = on_select.clone();
    let on_select_hidden = on_select.clone();

    view! {
        <div class="category-tabs">
            {base_categories.into_iter().map(|cat| {
                let on_select = on_select.clone();
                view! {
                    <button
                        class=move || {
                            let base = format!("category-tab {}", cat.class());
                            if active.get() == cat {
                                format!("{} active", base)
                            } else {
                                base
                            }
                        }
                        on:click=move |_| on_select(cat)
                    >
                        {cat.label()}
                    </button>
                }
            }).collect::<Vec<_>>()}
            <button
                class=move || {
                    let mut cls = String::from("category-tab mixes");
                    if !show_mixes.get() {
                        cls.push_str(" tab-hidden");
                    }
                    if active.get() == Category::Mixes {
                        cls.push_str(" active");
                    }
                    cls
                }
                on:click=move |_| on_select_mixes(Category::Mixes)
            >
                "Mixes"
            </button>
            <button
                class=move || {
                    let mut cls = String::from("category-tab hidden");
                    if !show_hidden.get() {
                        cls.push_str(" tab-hidden");
                    }
                    if active.get() == Category::Hidden {
                        cls.push_str(" active");
                    }
                    cls
                }
                on:click=move |_| on_select_hidden(Category::Hidden)
                title="Hidden channels"
            >
                "\u{1F441}"
            </button>
        </div>
    }
}
