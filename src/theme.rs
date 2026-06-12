//! 终端主题预置注册表（契约 D7：多预置 + 背景基调可调；base24 精神的槽位）。
//!
//! conmux 是主题数据的唯一属主——conflux 与未来的独立 CLI 形态共享同一套预置
//! （产品定位：conmux 可单用，见 conflux 侧 spec 2026-06-12-cool-craft-direction）。
//! 语义 pastel 前景（红/绿/黄/蓝/紫/青）按明暗两组固定；背景基调按预置切换。
//! agent truecolor 内容透传不经过此层（D8 颜色所有权分层）。
//!
//! 数据源：用户验收样稿 research/mux-theme-samples/b-backgrounds.html（2026-06-10
//! D7 裁决配套），六个预置原样收录。

use serde::{Deserialize, Serialize};

/// 明暗基调（消费端据此做对比度/光标策略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAppearance {
    Dark,
    Light,
}

/// 一个终端主题预置（16 ANSI + 基础槽位，hex 字符串 `#RRGGBB`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TerminalTheme {
    pub id: String,
    /// 展示名（含基调说明）
    pub name: String,
    pub appearance: ThemeAppearance,
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection_background: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

/// 默认预置（①蓝墨——2026-06-12 用户实机裁决：与 conflux 冷蓝黑壳同温）。
pub const DEFAULT_TERMINAL_THEME_ID: &str = "b-dark-ink";

/// 暗版语义前景（Catppuccin Macchiato pastel，固定）。
struct DarkSem;
impl DarkSem {
    const RED: &'static str = "#ED8796";
    const GREEN: &'static str = "#A6DA95";
    const YELLOW: &'static str = "#EED49F";
    const BLUE: &'static str = "#8AADF4";
    const MAGENTA: &'static str = "#C6A0F6";
    const CYAN: &'static str = "#8BD5CA";
    const BRIGHT_RED: &'static str = "#F0949F";
    const BRIGHT_GREEN: &'static str = "#B0E0A0";
    const BRIGHT_YELLOW: &'static str = "#F2DBAA";
    const BRIGHT_BLUE: &'static str = "#97B5F6";
    const BRIGHT_MAGENTA: &'static str = "#CFADF8";
    const BRIGHT_CYAN: &'static str = "#98DBD2";
}

/// 亮版语义前景（Catppuccin Latte，深 pastel——亮底才有对比，固定）。
struct LightSem;
impl LightSem {
    const RED: &'static str = "#D20F39";
    const GREEN: &'static str = "#40A02B";
    const YELLOW: &'static str = "#DF8E1D";
    const BLUE: &'static str = "#1E66F5";
    const MAGENTA: &'static str = "#8839EF";
    const CYAN: &'static str = "#179299";
    const BRIGHT_RED: &'static str = "#DE2D52";
    const BRIGHT_GREEN: &'static str = "#49B530";
    const BRIGHT_YELLOW: &'static str = "#EEA02D";
    const BRIGHT_BLUE: &'static str = "#3C7BF6";
    const BRIGHT_MAGENTA: &'static str = "#9A52F2";
    const BRIGHT_CYAN: &'static str = "#1FA8A9";
}

#[allow(clippy::too_many_arguments)]
fn dark(
    id: &str,
    name: &str,
    background: &str,
    foreground: &str,
    selection: &str,
    black: &str,
    white: &str,
    bright_black: &str,
    bright_white: &str,
) -> TerminalTheme {
    TerminalTheme {
        id: id.to_string(),
        name: name.to_string(),
        appearance: ThemeAppearance::Dark,
        background: background.to_string(),
        foreground: foreground.to_string(),
        cursor: "#F4DBD6".to_string(),
        selection_background: selection.to_string(),
        black: black.to_string(),
        red: DarkSem::RED.to_string(),
        green: DarkSem::GREEN.to_string(),
        yellow: DarkSem::YELLOW.to_string(),
        blue: DarkSem::BLUE.to_string(),
        magenta: DarkSem::MAGENTA.to_string(),
        cyan: DarkSem::CYAN.to_string(),
        white: white.to_string(),
        bright_black: bright_black.to_string(),
        bright_red: DarkSem::BRIGHT_RED.to_string(),
        bright_green: DarkSem::BRIGHT_GREEN.to_string(),
        bright_yellow: DarkSem::BRIGHT_YELLOW.to_string(),
        bright_blue: DarkSem::BRIGHT_BLUE.to_string(),
        bright_magenta: DarkSem::BRIGHT_MAGENTA.to_string(),
        bright_cyan: DarkSem::BRIGHT_CYAN.to_string(),
        bright_white: bright_white.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn light(
    id: &str,
    name: &str,
    background: &str,
    foreground: &str,
    selection: &str,
    black: &str,
    white: &str,
    bright_black: &str,
    bright_white: &str,
) -> TerminalTheme {
    TerminalTheme {
        id: id.to_string(),
        name: name.to_string(),
        appearance: ThemeAppearance::Light,
        background: background.to_string(),
        foreground: foreground.to_string(),
        cursor: "#DC8A78".to_string(),
        selection_background: selection.to_string(),
        black: black.to_string(),
        red: LightSem::RED.to_string(),
        green: LightSem::GREEN.to_string(),
        yellow: LightSem::YELLOW.to_string(),
        blue: LightSem::BLUE.to_string(),
        magenta: LightSem::MAGENTA.to_string(),
        cyan: LightSem::CYAN.to_string(),
        white: white.to_string(),
        bright_black: bright_black.to_string(),
        bright_red: LightSem::BRIGHT_RED.to_string(),
        bright_green: LightSem::BRIGHT_GREEN.to_string(),
        bright_yellow: LightSem::BRIGHT_YELLOW.to_string(),
        bright_blue: LightSem::BRIGHT_BLUE.to_string(),
        bright_magenta: LightSem::BRIGHT_MAGENTA.to_string(),
        bright_cyan: LightSem::BRIGHT_CYAN.to_string(),
        bright_white: bright_white.to_string(),
    }
}

/// 内置六预置（样稿 ①–⑥ 原样）。
pub fn builtin_terminal_themes() -> Vec<TerminalTheme> {
    vec![
        dark("b-dark-ink", "暗 · 蓝墨", "#1E2030", "#CAD3F5", "#363A4F", "#363A4F", "#B8C0E0", "#494D64", "#CAD3F5"),
        dark("b-dark-graphite", "暗 · 中性深灰", "#17191D", "#D2D4DA", "#2C2F36", "#2C3036", "#C4C7CE", "#474C54", "#E2E4E9"),
        dark("b-dark-near-black", "暗 · 近黑", "#0F1012", "#D6D8DC", "#26282D", "#23252A", "#C8CBD1", "#42454C", "#E6E8EB"),
        dark("b-dark-warm-charcoal", "暗 · 暖炭", "#1B1A17", "#D8D4CC", "#322F2A", "#322E28", "#C9C4BB", "#4D4840", "#E8E4DC"),
        light("b-light-paper", "亮 · 暖纸白", "#FAF6F0", "#4C4F69", "#E9E3D9", "#5C5F77", "#BCC0CC", "#6C6F85", "#DCE0E8"),
        light("b-light-cool-white", "亮 · 中性冷白", "#F6F7F9", "#494C5E", "#E4E7EB", "#5A5D72", "#BABEC8", "#6A6D80", "#DADDE4"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_have_unique_ids_and_contain_default() {
        let themes = builtin_terminal_themes();
        assert_eq!(themes.len(), 6);
        let mut ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 6, "id 必须唯一");
        assert!(themes.iter().any(|t| t.id == DEFAULT_TERMINAL_THEME_ID));
    }

    #[test]
    fn theme_serializes_snake_case_for_frontend() {
        let themes = builtin_terminal_themes();
        let json = serde_json::to_string(&themes[0]).unwrap();
        assert!(json.contains("\"selection_background\""));
        assert!(json.contains("\"bright_black\""));
        assert!(json.contains("\"appearance\":\"dark\""));
        // 默认蓝墨底色正确
        assert!(json.contains("#1E2030"));
    }
}
