//! 「从一页结果到下载清单」——**这一步是外壳的活**（`docs/01` §4）：
//! 选哪几个媒体、叫什么名字、放哪个目录，全在这里决定；
//! 组件只接受 `dest_dir` + `file_name` + `url`。
//!
//! 命名规则**刻意照抄参考实现**的默认模板
//! （`x-spider-mac/.../FileNameTemplate.swift` 的 `%POST_TIME% %USER_SCREEN_NAME%
//! %POST_ID%-%MEDIA_INDEX%%EXT%`，13 个变量里的 5 个），包括：
//! - 保留字符 `< > : " / \ | ? *` 与控制字符替换成 `!`；
//! - Windows 保留名（`con` / `nul` / `com1`…）后面加 `!`；
//! - 时间缺失时用 `未知日期`。
//!
//! 这么做不是为了"抄"，而是为了证明**命名这一层留在外壳、原样不动**——
//! 组件不碰文件名，接入时这段逻辑不需要搬家。

use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub job_id: String,
    pub post_id: String,
    pub media_id: String,
    pub kind: String,
    pub url: String,
    pub file_name: String,
    pub dest_dir: PathBuf,
    /// 探测到的字节数（`net.probe_size`）；`None` = 服务端没说。
    pub size: Option<u64>,
    /// 报告里给人看的时间与序号。
    pub post_time: String,
    pub media_index: usize,
    pub screen_name: String,
}

/// 一页的摘要（报告里要写清"取到了什么"）。
#[derive(Debug, Clone)]
pub struct PageSummary {
    pub posts: usize,
    pub medias: usize,
    pub end: bool,
    pub cursor: Option<String>,
}

impl PageSummary {
    pub fn says_more(&self) -> bool {
        self.cursor.is_some() && !self.end
    }
}

/// 从 `page<post>` 里挑出要下载的媒体。
///
/// 终止/筛选语义都留在外壳：这里只做"按类型过滤 + 取前 N 个"。
pub fn plan_from_page(
    page: &Value,
    screen_name: &str,
    user_name: &str,
    out_dir: &Path,
    subfolder_per_user: bool,
    wanted_kind: Option<&str>,
    limit: usize,
) -> Result<(Vec<Candidate>, PageSummary), String> {
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "page 里没有 items（形状不对）".to_string())?;
    let end = page.get("end").and_then(Value::as_bool).unwrap_or(false);
    let cursor = page
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut candidates = Vec::new();
    let mut media_total = 0usize;
    let mut dir = out_dir.to_path_buf();
    if subfolder_per_user {
        // 参考实现的 accountSubfolderEnabled：`<昵称>-@<用户名>`
        dir = dir.join(sanitize(&format!("{user_name}-@{screen_name}")));
    }

    for post in items {
        let post_id = post.get("id").and_then(Value::as_str).unwrap_or_default();
        let post_time = file_time(post.get("created_at").and_then(Value::as_str));
        let medias = post
            .get("medias")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        media_total += medias.len();
        for (index, media) in medias.iter().enumerate() {
            let kind = media.get("kind").and_then(Value::as_str).unwrap_or("");
            if let Some(wanted) = wanted_kind {
                if kind != wanted {
                    continue;
                }
            }
            let url = media.get("url").and_then(Value::as_str).unwrap_or_default();
            // 没有可下载 URL 的媒体不该进清单（真下载会立刻 404）
            if url.is_empty() {
                continue;
            }
            let media_id = media.get("id").and_then(Value::as_str).unwrap_or_default();
            let ext = media.get("ext").and_then(Value::as_str).unwrap_or("bin");
            let media_index = index + 1;
            // `job_id` 由**外壳**生成：它要能跨进程重启稳定，所以用稳定的业务 id
            // 而不是序号（序号会随筛选条件变化）——见 docs/CONTRACT.md §4.11
            let job_id = format!("{post_id}-{media_id}");
            let file_name = sanitize(&post_time)
                + " "
                + &sanitize(screen_name)
                + " "
                + &sanitize(post_id)
                + "-"
                + &media_index.to_string()
                + "."
                + ext;

            candidates.push(Candidate {
                job_id,
                post_id: post_id.to_string(),
                media_id: media_id.to_string(),
                kind: kind.to_string(),
                url: url.to_string(),
                file_name,
                dest_dir: dir.clone(),
                size: None,
                post_time: post_time.clone(),
                media_index,
                screen_name: screen_name.to_string(),
            });
            if candidates.len() >= limit {
                break;
            }
        }
        if candidates.len() >= limit {
            break;
        }
    }

    Ok((
        candidates,
        PageSummary {
            posts: items.len(),
            medias: media_total,
            end,
            cursor,
        },
    ))
}

/// RFC3339 → 参考实现的文件名时间格式 `yyyy-MM-dd HH-mm-ss`。
///
/// **时区**：组件给的是 UTC（`docs/02` §D3：组件不做时区换算）。
/// 参考实现在外壳里格式化成**本地**时间——要保持文件名与旧行为一致，
/// 外壳需要在这里自己做 UTC→本地 的转换（这一步留在外壳，正是它该在的地方）。
fn file_time(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        // 参考实现在时间缺失时用的就是这四个字
        return "未知日期".to_string();
    };
    let (date, rest) = match raw.split_once('T') {
        Some(parts) => parts,
        // 不是 RFC3339 就原样用，别丢信息
        None => return raw.to_string(),
    };
    let time = rest
        .split(['Z', '+', '-'])
        .next()
        .unwrap_or(rest)
        .split('.')
        .next()
        .unwrap_or(rest)
        .replace(':', "-");
    format!("{date} {time}")
}

/// 参考实现的 `unicodeFilenamify`：保留字符与控制字符 → `!`，Windows 保留名加 `!`。
pub fn sanitize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        let reserved = matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || (ch as u32) < 0x20;
        out.push(if reserved { '!' } else { ch });
    }
    let lower = out.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    let windows_reserved = ["con", "prn", "aux", "nul"].contains(&stem)
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem[3..].chars().all(|c| c.is_ascii_digit()));
    if windows_reserved {
        out.push('!');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page() -> Value {
        json!({
            "items": [
                { "id": "111", "created_at": "2009-09-30T12:34:56Z",
                  "medias": [ { "kind": "video", "id": "m1", "url": "https://v/a.mp4", "ext": "mp4" },
                              { "kind": "photo", "id": "m2", "url": "https://p/b.jpg", "ext": "jpg" } ] },
                { "id": "222", "created_at": null,
                  "medias": [ { "kind": "video", "id": "m3", "url": "https://v/c.mp4", "ext": "mp4" } ] }
            ],
            "cursor": "CUR",
            "end": false
        })
    }

    #[test]
    fn plans_in_page_order_and_names_like_the_reference_template() {
        let (plan, summary) = plan_from_page(
            &page(),
            "demo_user",
            "Demo User",
            Path::new("/tmp/out"),
            false,
            None,
            3,
        )
        .unwrap();
        assert_eq!(plan.len(), 3);
        assert_eq!(summary.posts, 2);
        assert_eq!(summary.medias, 3);
        assert!(summary.says_more());

        assert_eq!(plan[0].file_name, "2009-09-30 12-34-56 demo_user 111-1.mp4");
        assert_eq!(plan[1].file_name, "2009-09-30 12-34-56 demo_user 111-2.jpg");
        // 时间缺失时参考实现用的是「未知日期」
        assert_eq!(plan[2].file_name, "未知日期 demo_user 222-1.mp4");
        // job_id 由外壳生成，且必须跨重启稳定（用业务 id，不用序号）
        assert_eq!(plan[0].job_id, "111-m1");
        assert_eq!(plan[2].dest_dir, PathBuf::from("/tmp/out"));
    }

    #[test]
    fn kind_filter_and_limit_apply_before_naming() {
        let (plan, _) = plan_from_page(
            &page(),
            "demo_user",
            "Demo",
            Path::new("/o"),
            false,
            Some("photo"),
            5,
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, "photo");
        assert_eq!(plan[0].media_index, 2, "序号是它在原推文媒体列表里的位置");

        let (plan, _) = plan_from_page(
            &page(),
            "demo_user",
            "Demo",
            Path::new("/o"),
            false,
            None,
            1,
        )
        .unwrap();
        assert_eq!(plan.len(), 1, "--count 就是上限");
    }

    #[test]
    fn subfolder_per_user_matches_the_reference_naming() {
        let (plan, _) = plan_from_page(
            &page(),
            "demo_user",
            "Demo User",
            Path::new("/o"),
            true,
            None,
            1,
        )
        .unwrap();
        assert_eq!(plan[0].dest_dir, PathBuf::from("/o/Demo User-@demo_user"));
    }

    #[test]
    fn sanitize_replaces_reserved_chars_and_windows_names() {
        assert_eq!(sanitize("a<b>c:d\"e/f\\g|h?i*j"), "a!b!c!d!e!f!g!h!i!j");
        assert_eq!(sanitize("con"), "con!");
        assert_eq!(sanitize("COM1"), "COM1!");
        assert_eq!(sanitize("console"), "console", "只有整个名字才是保留名");
        assert_eq!(sanitize("中文 名字"), "中文 名字", "非 ASCII 不动");
    }

    #[test]
    fn file_time_handles_offsets_and_fractions() {
        assert_eq!(
            file_time(Some("2009-09-30T12:34:56Z")),
            "2009-09-30 12-34-56"
        );
        assert_eq!(
            file_time(Some("2009-09-30T12:34:56.123Z")),
            "2009-09-30 12-34-56"
        );
        assert_eq!(
            file_time(Some("2009-09-30T12:34:56+08:00")),
            "2009-09-30 12-34-56"
        );
        assert_eq!(file_time(None), "未知日期");
        assert_eq!(file_time(Some("garbage")), "garbage");
    }

    #[test]
    fn media_without_a_url_is_not_planned() {
        let page = json!({ "items": [ { "id": "1", "created_at": null,
            "medias": [ { "kind": "video", "id": "x", "url": "", "ext": "mp4" } ] } ], "end": true });
        let (plan, _) = plan_from_page(&page, "u", "U", Path::new("/o"), false, None, 3).unwrap();
        assert!(plan.is_empty(), "没有 URL 的媒体不该进清单");
    }
}
