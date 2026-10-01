#!/usr/bin/env python3
"""把 live 录制的**原始**响应脱敏成可入库的 fixture。

    python3 script/redact_fixtures.py [--check]

为什么分两步（录制 → 脱敏），而不是录完直接入库：
`docs/04-TESTING-AND-FIXTURES.md` §2.1 要求 fixture 必须是**真实抓到的响应**，
同时要求**脱敏**。把"抓"和"洗"拆开，两件事就各自可核对：
录制保证真实性，这个脚本保证不含个人数据，而且规则是**可审计**的。

脱敏规则（与 docs/04 §2.1 一一对应）：

- 凭据 / 事务号 → 整段换成 "REDACTED"；
- 用户名 / 昵称 / 正文 / 简介 / 地点 → 固定假值（保留字符类型与近似长度）；
- 数字 id（`rest_id` / `*_id` / `*_id_str` / `*_ids`）→ **定长的假数字，且互不相同**：
  长度保留（下游有按字符串处理的代码），而"不同 id 洗成同一个"会让
  **去重与分页语义在 fixture 上失真**（实测踩过：一页里的推文全变成同一个 id，
  于是去重把整页干掉，所有聚合测试一起红）；
- 剪不断的兜底：**任何位置**出现的长数字串（≥10 位，例如 `entryId` 里的推文 id、
  `media_key`、数组元素里的 id）都按同一个映射替换。枚举字段名迟早会漏——
  实测漏过 `entryId`、`pinned_tweet_ids_str[]`、`edit_tweet_ids[]`；
- 计数（`*_count` / `count`）→ 按 key 决定性的小假数（不泄露真实量级）；
- 时间 → 固定假时间（保留 Twitter 时间格式）；
- 媒体/头像 URL → **保留 host**、路径换成假名（不把真实 URL 提交进仓库）；
- 数组裁到 3 条以内，**但带 `cursorType` 的条目一定保留**——
  游标才是分页语义的关键，裁掉它整套分页测试就失去意义；
- **不删字段**——删字段等于降低解析覆盖率；
- 裁剪痕迹记在 fixture 顶层的 `redaction` 里，**不塞进 body**，
  这样 `response.body` 始终是结构真实的响应。

脚本自己会做**四条事后断言**（实测每一条都抓到过东西）：
1. 原始个人数据不得出现在输出里（screen_name / 正文）；
2. 原始的长数字串一个都不许留下来（抓"漏洗 id"）；
3. 不同的原始 id 不许洗成同一个假值（抓"映射冲突"）；
4. `match.cursor` 取值合法（写错会让回放路由静默失效）。
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
FIXTURE_ROOT = ROOT / "fixtures"

FAKE_SCREEN_NAME = "demo_user"
FAKE_DISPLAY_NAME = "Demo User"
FAKE_DESCRIPTION = "REDACTED demo bio for fixture testing purposes only."
FAKE_POST_TEXT = "REDACTED post text."
FAKE_PLACE = "Redacted Place"
FAKE_COUNTRY = "XX"
FAKE_CREATED_AT = "Wed Sep 30 12:34:56 +0000 2009"
FAKE_TIMESTAMP_MS = "1700000000000"
FAKE_BIRTHDATE = {"day": 1, "month": 1, "year": 2000}
FAKE_MEDIA_HOSTS = {"pbs.twimg.com", "video.twimg.com", "abs.twimg.com"}

TEXT_KEYS = {
    "screen_name": FAKE_SCREEN_NAME,
    "name": FAKE_DISPLAY_NAME,
    "full_name": FAKE_PLACE,
    "description": FAKE_DESCRIPTION,
    "bio": FAKE_DESCRIPTION,
    "full_text": FAKE_POST_TEXT,
    "text": FAKE_POST_TEXT,
    "location": "",
    "country": FAKE_COUNTRY,
    "country_code": FAKE_COUNTRY,
    "place_name": FAKE_PLACE,
    "alt_text": "REDACTED alt text.",
    "ext_alt_text": "REDACTED alt text.",
    "title": "REDACTED title.",
}
URL_KEYS = {
    "url",
    "expanded_url",
    "display_url",
    "profile_image_url_https",
    "profile_banner_url",
    "profile_image_url",
    "media_url_https",
    "unified_url",
    "vanity_url",
    # 新版用户结构把头像放在 avatar.image_url
    # （实测：search / home / following 用的是这个形状）
    "image_url",
    "image_url_https",
}
COUNT_KEYS = {
    "followers_count": 12,
    "friends_count": 34,
    "listed_count": 1,
    "media_count": 56,
    "statuses_count": 78,
    "favourites_count": 90,
    "fast_followers_count": 0,
    "normal_followers_count": 12,
    "bookmark_count": 0,
    "favorite_count": 0,
    "reply_count": 0,
    "retweet_count": 0,
    "quote_count": 0,
}
SECRET_KEYS = {
    "cookie",
    "ct0",
    "auth_token",
    "csrf",
    "token",
    "authorization",
    "x-csrf-token",
}
TRACE_HEADER_KEYS = {"x-transaction-id", "x-client-transaction-id", "x-response-time"}
MAX_ARRAY = 3
LONG_DIGITS = re.compile(r"\d{10,}")


def fake_count(key: str) -> int:
    """按 key 决定性的小假数：稳定，且不泄露真实量级。"""
    return zlib.crc32(key.encode()) % 100


def fake_numeric_id(value: str) -> str:
    """定长的假数字 id。

    三条性质缺一不可：
    1. **纯数字**（下游按字符串解析数字 id）；
    2. **长度与原来一致**（便于人眼比对，也保留按长度做启发式的代码的行为）；
    3. **互不相同**——同长度的不同 id 不能洗成同一个值：否则去重会把整页干掉，
       分页测试也会退化。所以用值的 crc32 播一个确定性 LCG 生成数字。
    """
    if not value:
        return value
    state = zlib.crc32(value.encode()) or 1
    digits = []
    for _ in range(len(value)):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        digits.append(str(state % 10))
    if digits[0] == "0":
        digits[0] = "1"  # 保留"首位非 0"
    return "".join(digits)


def fake_digit_runs(text: str) -> str:
    """把文本里 ≥10 位的连续数字替换成它的假值。

    为什么需要这条**按值**的规则：真实 id 会藏在各种我们料不到的字段里
    （实测漏过 `entryId`（`tweet-<id>`）、`pinned_tweet_ids_str[]`、
    `edit_tweet_ids[]`、`media_key`）。枚举字段名迟早会漏，按"长数字串"来洗不会。
    """
    return LONG_DIGITS.sub(lambda m: fake_numeric_id(m.group(0)), text)


def fake_url(value: str) -> str:
    """保留 host（解析代码可能按 host 分支），路径换成假名。"""
    if not value.startswith("http"):
        return "https://example.invalid/redacted"
    rest = value[len("https://") :] if value.startswith("https://") else value[len("http://") :]
    host = rest.split("/", 1)[0]
    if host in FAKE_MEDIA_HOSTS:
        suffix = ".mp4" if ".mp4" in value else ".jpg"
        return f"https://{host}/redacted/demo{suffix}"
    return "https://example.invalid/redacted"


def contains_cursor(node) -> bool:
    """节点里是否带游标（裁剪数组时据此**一定保留**游标条目）。"""
    if isinstance(node, dict):
        if "cursorType" in node:
            return True
        return any(contains_cursor(v) for v in node.values())
    if isinstance(node, list):
        return any(contains_cursor(v) for v in node)
    return False


def is_id_key(lowered: str) -> bool:
    """哪些键装的是"X 的数字 id"（含复数形式的数组键）。"""
    return (
        lowered in ("rest_id", "id_str", "id", "twitter_id", "user_id")
        or lowered.endswith("_id")
        or lowered.endswith("_id_str")
        or lowered.endswith("_ids")
        or lowered.endswith("_ids_str")
    )


def redact_string(key: str, value: str, known_names: set[str], path: str = "") -> str:
    lowered = key.lower()

    if lowered in SECRET_KEYS:
        return "REDACTED"
    if lowered in TEXT_KEYS:
        return TEXT_KEYS[lowered]
    # 数据驱动的兜底：**任何**位置上出现的真实用户名都换掉。
    # 为什么需要它：X 把用户名放在很多料不到的字段里（实测踩到过
    # `in_reply_to_screen_name`——"回复 @xxx" 用的那个，docs/02 §C2）。
    # 靠"枚举字段名"迟早漏，靠"已知值集合"不会。
    if value in known_names:
        return FAKE_SCREEN_NAME
    if lowered.endswith("_screen_name"):
        return FAKE_SCREEN_NAME
    # 卡片里的自由文本（binding_values）能放任意内容
    if "binding_values" in path and lowered in ("string_value", "value"):
        return "REDACTED card value."
    if lowered in URL_KEYS or (lowered.endswith("_url") and lowered != "url_type"):
        return fake_url(value)
    if value.startswith("http://") or value.startswith("https://"):
        # 数组元素之类的裸字符串：只要长得像 URL 就按 URL 洗
        return fake_url(value)
    if lowered == "created_at":
        return FAKE_CREATED_AT
    if is_id_key(lowered):
        # 数字 id → 定长假数字；非数字（v1 的 base64 id）→ 保类型、保互不相同
        return (
            fake_numeric_id(value)
            if value.isdigit()
            else f"redacted-{zlib.crc32(value.encode()):08x}"
        )
    if lowered.endswith("_msec") or lowered.endswith("_ms"):
        return FAKE_TIMESTAMP_MS if value.isdigit() else value
    if lowered.endswith("_count") and value.isdigit():
        return str(fake_count(lowered))
    # 最后一道：洗掉藏在任意字段里的长数字串。
    # **顺序很关键**：放在 id 规则之后，才不会让 rest_id 走通用路径而丢掉
    # "非数字 id" 的处理；放在最后，则任何漏网字段都还会被过一遍。
    return fake_digit_runs(value)


def redact(node, notes: list, known_names: set[str], path: str = ""):
    if isinstance(node, dict):
        out = {}
        for key, value in node.items():
            lowered = key.lower()
            child_path = f"{path}.{key}" if path else key
            if lowered in SECRET_KEYS:
                out[key] = "REDACTED"
            elif isinstance(value, bool):
                out[key] = value
            elif isinstance(value, (int, float)):
                # **数字也要洗**：实测漏过（`rest_id` 有时是数字），
                # 而"数字值不处理"是很容易被忽略的一条分支
                out[key] = redact_number(key, value, child_path)
            elif isinstance(value, str):
                out[key] = redact_string(key, value, known_names, child_path)
            else:
                out[key] = redact(value, notes, known_names, child_path)
        return out

    if isinstance(node, list):
        kept = []
        for index, item in enumerate(node):
            # 游标条目一定保留：裁掉它，分页测试就没意义了
            if index >= MAX_ARRAY and not contains_cursor(item):
                continue
            if isinstance(item, str):
                # **数组里的裸字符串**也必须洗——实测漏过
                # `pinned_tweet_ids_str[]` 与 `edit_tweet_ids[]`
                kept.append(redact_string("", item, known_names, f"{path}[{index}]"))
            else:
                kept.append(redact(item, notes, known_names, f"{path}[{index}]"))
        if len(kept) != len(node):
            notes.append(f"{path or '<root>'}：{len(node)} 条 → {len(kept)} 条（游标条目已保留）")
        return kept

    if isinstance(node, str):
        # 顶层或其它位置的裸字符串
        return redact_string("", node, known_names, path)

    return node


def redact_number(key: str, value, path: str):
    lowered = key.lower()
    digits = str(int(value))
    if lowered in COUNT_KEYS:
        return COUNT_KEYS[lowered]
    if lowered == "count" or lowered.endswith("_count"):
        return fake_count(lowered)
    if lowered in ("day", "month", "year"):
        return FAKE_BIRTHDATE.get(lowered, value)
    if is_id_key(lowered) and len(digits) >= 8:
        # 保留"是数字"这个类型，同时换成假值
        return int(fake_numeric_id(digits))
    return value


def find_long_digit_runs(node, out: set | None = None) -> set:
    """响应里所有 ≥10 位的数字串（用于校验"没漏洗"与"映射不冲突"）。"""
    if out is None:
        out = set()
    if isinstance(node, dict):
        for value in node.values():
            find_long_digit_runs(value, out)
    elif isinstance(node, list):
        for item in node:
            find_long_digit_runs(item, out)
    elif isinstance(node, str):
        out.update(LONG_DIGITS.findall(node))
    return out


def collect_screen_names(node, out: set):
    """收集响应里出现的**所有**真实用户名（含 `in_reply_to_screen_name` 这类）。"""
    if isinstance(node, dict):
        for key, value in node.items():
            if (
                isinstance(value, str)
                and (key == "screen_name" or key.endswith("_screen_name"))
                and value.strip()
            ):
                out.add(value)
            collect_screen_names(value, out)
    elif isinstance(node, list):
        for item in node:
            collect_screen_names(item, out)


def collect_texts(node, out: set):
    if isinstance(node, dict):
        for key, value in node.items():
            if key in ("full_text", "description") and isinstance(value, str) and value.strip():
                out.add(value)
            collect_texts(value, out)
    elif isinstance(node, list):
        for item in node:
            collect_texts(item, out)


def body_screen_name(body) -> str | None:
    """响应里那个"被查询的"用户（UserByScreenName 的 data.user.result.legacy.screen_name）。"""
    try:
        return body["data"]["user"]["result"]["legacy"]["screen_name"]
    except (KeyError, TypeError):
        return None


def build_match(scenario: str, hint: dict | None, redacted_screen_name: str | None) -> dict:
    """生成回放路由条件。

    优先用录制时写下的 `match_hint`（录制者知道这一条是什么场景），
    再按需补上 screen_name——**这一步很关键**：
    如果 `normal` 不约束 screen_name，它就与 `not_found` 同样具体，
    而未命中的请求会按文件名顺序落到 `normal` 上，于是"用户不存在"这条用例
    就会**静默地测到别的东西**（这正是它第一次跑出来的样子）。
    """
    match = dict(hint) if hint else {"method": "GET"}
    if redacted_screen_name and "screen_name" not in match:
        match["screen_name"] = redacted_screen_name
    if not hint:
        match.setdefault("auth", "none" if scenario == "unauthorized" else "valid")
    return match


def redact_headers(headers: dict) -> dict:
    return {k: ("REDACTED" if k.lower() in TRACE_HEADER_KEYS else v) for k, v in headers.items()}


def process(raw_path: pathlib.Path, check_only: bool) -> tuple[pathlib.Path, list[str]]:
    raw = json.loads(raw_path.read_text())
    scenario = raw["scenario"]
    endpoint = raw["endpoint"]
    body = raw["response"]["body"]

    original_names: set[str] = set()
    collect_screen_names(body, original_names)
    original_texts: set[str] = set()
    collect_texts(body, original_texts)

    notes: list[str] = []
    redacted_body = redact(body, notes, original_names)

    fixture = {
        "endpoint": endpoint,
        "scenario": scenario,
        "captured_at": raw.get("captured_at"),
        "note": (
            "真实响应经 script/redact_fixtures.py 脱敏；结构完整、个人数据已替换为固定假值"
            "（见 docs/04 §2.1）"
        ),
        "match": build_match(scenario, raw.get("match_hint"), body_screen_name(redacted_body)),
        "response": {
            "status": raw["response"]["status"],
            "headers": redact_headers(raw["response"].get("headers", {})),
            "body": redacted_body,
        },
    }
    if notes:
        fixture["redaction"] = {"arrays_trimmed": notes}

    problems: list[str] = []

    # 断言 1：**原始的长数字串一个都不许留下来**（专抓"漏洗 id"）
    leftovers = find_long_digit_runs(body) & find_long_digit_runs(redacted_body)
    if leftovers:
        problems.append(
            f"{len(leftovers)} 个原始数字串原封不动留在了输出里：{sorted(leftovers)[:3]}…"
        )

    # 断言 2：不同的原始数字串不许洗成同一个假值（会让去重/分页语义失真）
    seen_map: dict[str, str] = {}
    for original in find_long_digit_runs(body):
        faked = fake_numeric_id(original)
        if seen_map.setdefault(faked, original) != original:
            problems.append(
                f"两个不同的原始 id（{original} 与 {seen_map[faked]}）洗成了同一个假值 {faked}"
            )

    # 断言 3：原始个人数据不得出现在输出里
    rendered = json.dumps(fixture, ensure_ascii=False)
    for name in original_names:
        if name and name != FAKE_SCREEN_NAME and f'"{name}"' in rendered:
            problems.append(f"screen_name {name!r} 仍出现在脱敏结果里")
    for text in original_texts:
        if len(text) > 12 and text in rendered:
            problems.append(f"正文片段仍出现：{text[:24]!r}…")

    # 断言 4：cursor 条件合法（写错会让回放路由静默失效）
    cursor = fixture["match"].get("cursor")
    if cursor is not None and cursor not in ("absent", "present"):
        problems.append(f"match.cursor 取值非法：{cursor!r}")

    out_path = FIXTURE_ROOT / endpoint / f"{scenario}.json"
    if not check_only:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n")
    return out_path, problems


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--check", action="store_true", help="只校验，不写文件（CI 用）")
    args = parser.parse_args()

    raws = sorted(FIXTURE_ROOT.glob("*/raw/*.json"))
    if not raws:
        print("没有找到任何 fixtures/**/raw/*.json —— 先跑录制：", file=sys.stderr)
        print(
            "  XSPIDER_LIVE=1 XSPIDER_COOKIE=... cargo test -p xspider-fetch"
            " --test record_live -- --ignored",
            file=sys.stderr,
        )
        return 1

    failed = False
    total_before = total_after = 0
    for raw_path in raws:
        before = raw_path.stat().st_size
        out_path, problems = process(raw_path, args.check)
        after = out_path.stat().st_size if out_path.exists() else 0
        total_before += before
        total_after += after
        rel_out = out_path.relative_to(ROOT)
        if problems:
            failed = True
            print(f"[!] {raw_path.name} → {rel_out}")
            for problem in problems:
                print(f"    问题：{problem}")
        else:
            verb = "校验通过" if args.check else "已写入"
            print(f"[✓] {raw_path.name} → {rel_out}（{verb}，{before // 1024}KB → {after // 1024}KB）")

    print(f"\n合计：raw {total_before // 1024}KB → 入库 {total_after // 1024}KB")
    if failed:
        print("脱敏自检失败：不要提交上面的文件。", file=sys.stderr)
        return 1
    print("下一步：检查 fixtures/ 下的结果，确认结构与 match 条件合理；raw/ 不进仓库。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
