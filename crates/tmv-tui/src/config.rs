//! 用户配置：读写 `$XDG_CONFIG_HOME/tmv/config.toml`（缺省 `~/.config/tmv/config.toml`）。
//!
//! 文件是 TOML 的最小子集：一行一个 `key = value`，`#` 之后为注释。
//! 解析保持宽松——未知键与非法值一律忽略并沿用默认值，避免手改出错导致无法启动。

use std::path::PathBuf;
use std::time::Duration;

use tmv_core::{GroupBy, Metric, SortOrder};

/// 默认刷新间隔（秒）。
const DEFAULT_INTERVAL_SECS: u64 = 5;

/// 允许写回的最大间隔，避免手改配置时写出离谱的值。
const MAX_INTERVAL_SECS: u64 = 3600;

/// 持久化的界面偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub metric: Metric,
    pub group_by: GroupBy,
    pub sort: SortOrder,
    pub interval: Duration,
    pub auto_refresh: bool,
    pub pages: bool,
    pub diff: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            metric: Metric::Pss,
            group_by: GroupBy::Category,
            sort: SortOrder::SizeDesc,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            auto_refresh: false,
            pages: true,
            diff: true,
        }
    }
}

impl Config {
    /// 配置文件路径：优先 `TMV_CONFIG`，其次 `XDG_CONFIG_HOME`，最后 `~/.config`。
    pub fn path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("TMV_CONFIG") {
            return Some(PathBuf::from(path));
        }
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("tmv").join("config.toml"))
    }

    /// 读取配置；文件不存在或全部键非法时返回默认值。
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// 解析配置文本；非法键值沿用默认值。
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "metric" => {
                    if let Some(metric) = parse_metric(value) {
                        config.metric = metric;
                    }
                }
                "group" => {
                    if let Some(group) = parse_group(value) {
                        config.group_by = group;
                    }
                }
                "sort" => {
                    if let Some(sort) = parse_sort(value) {
                        config.sort = sort;
                    }
                }
                "interval" => {
                    if let Ok(secs) = value.parse::<u64>() {
                        if (1..=MAX_INTERVAL_SECS).contains(&secs) {
                            config.interval = Duration::from_secs(secs);
                        }
                    }
                }
                "auto_refresh" => {
                    if let Some(value) = parse_bool(value) {
                        config.auto_refresh = value;
                    }
                }
                "pages" => {
                    if let Some(value) = parse_bool(value) {
                        config.pages = value;
                    }
                }
                "diff" => {
                    if let Some(value) = parse_bool(value) {
                        config.diff = value;
                    }
                }
                _ => {}
            }
        }
        config
    }

    /// 序列化为配置文件文本。
    pub fn to_text(&self) -> String {
        format!(
            "# tmv 的界面偏好，由 tmv 退出时自动写入，可手工编辑。\n\
             # metric: virtual | rss | pss | private | shared | swap\n\
             metric = \"{}\"\n\
             # group: category | file | map\n\
             group = \"{}\"\n\
             # sort: size | address\n\
             sort = \"{}\"\n\
             # 定时刷新间隔（秒），命令行 --interval 优先。\n\
             interval = {}\n\
             auto_refresh = {}\n\
             pages = {}\n\
             diff = {}\n",
            metric_name(self.metric),
            group_name(self.group_by),
            sort_name(self.sort),
            self.interval.as_secs(),
            self.auto_refresh,
            self.pages,
            self.diff,
        )
    }

    /// 写回配置文件，返回实际写入的路径。
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::path()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "无法确定配置目录"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_text())?;
        Ok(path)
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_metric(value: &str) -> Option<Metric> {
    match value {
        "virtual" => Some(Metric::Virtual),
        "rss" => Some(Metric::Rss),
        "pss" => Some(Metric::Pss),
        "private" => Some(Metric::Private),
        "shared" => Some(Metric::Shared),
        "swap" => Some(Metric::Swap),
        _ => None,
    }
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Virtual => "virtual",
        Metric::Rss => "rss",
        Metric::Pss => "pss",
        Metric::Private => "private",
        Metric::Shared => "shared",
        Metric::Swap => "swap",
    }
}

fn parse_group(value: &str) -> Option<GroupBy> {
    match value {
        "category" => Some(GroupBy::Category),
        "file" => Some(GroupBy::File),
        "map" => Some(GroupBy::Map),
        _ => None,
    }
}

fn group_name(group: GroupBy) -> &'static str {
    match group {
        GroupBy::Category => "category",
        GroupBy::File => "file",
        GroupBy::Map => "map",
    }
}

fn parse_sort(value: &str) -> Option<SortOrder> {
    match value {
        "size" => Some(SortOrder::SizeDesc),
        "address" => Some(SortOrder::Address),
        _ => None,
    }
}

fn sort_name(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::SizeDesc => "size",
        SortOrder::Address => "address",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_all_fields() {
        let config = Config {
            metric: Metric::Rss,
            group_by: GroupBy::File,
            sort: SortOrder::Address,
            interval: Duration::from_secs(30),
            auto_refresh: true,
            pages: false,
            diff: false,
        };
        assert_eq!(Config::parse(&config.to_text()), config);
    }

    #[test]
    fn unknown_keys_and_comments_are_ignored() {
        let config = Config::parse(
            "# 顶部注释\n\
             unknown = 1\n\
             metric = \"rss\"   # 行尾注释\n\
             \n\
             这行没有等号\n",
        );
        assert_eq!(config.metric, Metric::Rss);
        assert_eq!(config.group_by, Config::default().group_by);
    }

    #[test]
    fn invalid_values_keep_defaults() {
        let default = Config::default();
        let config = Config::parse(
            "metric = \"nope\"\n\
             group = \"nope\"\n\
             sort = \"nope\"\n\
             interval = 0\n\
             auto_refresh = yes\n",
        );
        assert_eq!(config.metric, default.metric);
        assert_eq!(config.group_by, default.group_by);
        assert_eq!(config.sort, default.sort);
        assert_eq!(config.interval, default.interval);
        assert_eq!(config.auto_refresh, default.auto_refresh);
    }

    #[test]
    fn interval_has_upper_bound() {
        let config = Config::parse(&format!("interval = {}", MAX_INTERVAL_SECS + 1));
        assert_eq!(config.interval, Config::default().interval);
    }
}
