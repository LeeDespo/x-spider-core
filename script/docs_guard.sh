#!/usr/bin/env bash
# 文档护栏（docs guard）——接入 .github/workflows/ci.yml 的「文档护栏」job。
# 逐项守卫仓库文档的入库与一致性约束，任何一项不过即整体失败：
#   a. AGENTS.md 已被 git 跟踪（它是仓库正式开发规范，必须入库）；
#   b. AGENTS.md 未被 .gitignore 忽略；
#   c. 活跃文档（README.md、AGENTS.md、docs/*.md）不得引用 docs/00-KICKOFF.md——
#      它不随仓库分发，活跃文档引用它等于指向一个新 clone 里不存在的文件。
#      （docs/history/ 整个目录与 docs/00-KICKOFF.md 本身不在扫描范围。）
#   d. README.md 与 AGENTS.md 的 Markdown 相对链接：目标必须存在、不得被
#      .gitignore 忽略、且不得指向 docs/00-KICKOFF.md 或 ACCEPTANCE.md。
#   e. 契约版本同步：README 的「当前契约版本：X.Y.Z」与 docs/CONTRACT.md 的
#      「契约版本：**X.Y.Z**」（带星号的加粗写法）必须是同一个版本号。
# 用法：bash script/docs_guard.sh（脚本自动切到仓库根，任意目录可运行）。
# 注意：AGENTS.md 尚未提交时检查 a 会失败——这是护栏本意，不是误报。
# 写法约定：双引号里紧跟中文标点的变量引用一律写作 ${var}——bash 在某些 locale
# 下会把紧贴变量名的多字节字符并进变量名（$var）→ unbound variable）。
set -euo pipefail

errors=0

report_fail() {
  errors=$((errors + 1))
  echo "  失败 —— $*" >&2
}

# 统一以仓库根为工作目录，之后所有路径都是仓库相对路径。
repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "文档护栏：无法执行——当前目录不在 git 仓库内。" >&2
  exit 1
}
cd "${repo_root}"

echo "文档护栏：开始逐项检查（仓库根：${repo_root}）"

# ---------- 检查 a：AGENTS.md 已被 git 跟踪 ----------
echo "检查 a：AGENTS.md 已被 git 跟踪"
if git ls-files --error-unmatch AGENTS.md >/dev/null 2>&1; then
  echo "  通过"
else
  report_fail "AGENTS.md 未被 git 跟踪。它是仓库正式开发规范，必须 git add AGENTS.md 后提交。"
fi

# ---------- 检查 b：AGENTS.md 未被 .gitignore 忽略 ----------
echo "检查 b：AGENTS.md 未被 .gitignore 忽略"
if git check-ignore -q AGENTS.md; then
  report_fail "AGENTS.md 命中了 .gitignore 规则。请从 .gitignore 移除对 /AGENTS.md 的忽略（仓库规范文件必须入库）。"
else
  echo "  通过"
fi

# ---------- 检查 c：活跃文档不引用 docs/00-KICKOFF.md ----------
echo "检查 c：活跃文档（README.md、AGENTS.md、docs/*.md）不引用 docs/00-KICKOFF.md"
c_bad=0
for f in README.md AGENTS.md docs/*.md; do
  if [ "${f}" = "docs/00-KICKOFF.md" ]; then continue; fi  # 它本身不是「引用者」
  if [ ! -f "${f}" ]; then continue; fi
  if grep -nH '00-KICKOFF' -- "${f}"; then
    report_fail "${f} 引用了 docs/00-KICKOFF.md（行号见上）。该文件不随仓库分发，活跃文档不得依赖或链接它。"
    c_bad=1
  fi
done
if [ "${c_bad}" -eq 0 ]; then
  echo "  通过"
fi

# ---------- 检查 d：README.md / AGENTS.md 的 Markdown 相对链接 ----------
echo "检查 d：README.md / AGENTS.md 的 Markdown 相对链接（目标存在、不被忽略、不含 00-KICKOFF/ACCEPTANCE）"
d_bad=0
for md in README.md AGENTS.md; do
  if [ ! -f "${md}" ]; then
    report_fail "找不到 ${md}。"
    d_bad=1
    continue
  fi
  dir=$(dirname "${md}")
  # 提取行内链接目标 [..](target)：剥掉 "]((" 与 ")"，顺手剥掉 < > 包裹
  links=$(grep -oE '\]\([^)]+\)' "${md}" | sed -E 's/^\]\(//; s/\)$//; s/^<//; s/>$//') || links=""
  if [ -z "${links}" ]; then
    echo "  ${md}：未发现 Markdown 链接。"
    continue
  fi
  while IFS= read -r target; do
    if [ -z "${target}" ]; then continue; fi
    # 去掉可选的链接标题：[text](path "title")
    target=${target%%[[:space:]]*}
    # 跳过外链与纯锚点
    case "${target}" in
      http://*|https://*|mailto:*|\#*) continue ;;
    esac
    # 去掉 #anchor（去完为空说明原本就是纯锚点）
    target=${target%%#*}
    if [ -z "${target}" ]; then continue; fi
    # 指向被淘汰/本地忽略文档的链接直接判失败
    case "${target}" in
      *docs/00-KICKOFF.md*|*ACCEPTANCE.md*)
        report_fail "${md} 链接到了不允许的目标：${target}（不得链接 docs/00-KICKOFF.md 或 ACCEPTANCE.md）。"
        d_bad=1
        continue
        ;;
    esac
    # 以文件所在目录为基准解析；以 / 开头的按仓库根解析
    case "${target}" in
      /*) rel=${target#/} ;;
      *)  rel=${dir}/${target} ;;
    esac
    rel=${rel#./}
    if [ ! -e "${rel}" ]; then
      report_fail "${md} 的相对链接目标不存在：${target}（解析为 ${rel}）。"
      d_bad=1
      continue
    fi
    if git check-ignore -q -- "${rel}"; then
      report_fail "${md} 的链接目标被 .gitignore 忽略：${target}（解析为 ${rel}）。"
      d_bad=1
    fi
  done <<< "${links}"
done
if [ "${d_bad}" -eq 0 ]; then
  echo "  通过"
fi

# ---------- 检查 e：契约版本同步 ----------
echo "检查 e：README「当前契约版本：」与 docs/CONTRACT.md「契约版本：」一致"
readme_vers=$(grep -o '当前契约版本：[0-9.]*' README.md 2>/dev/null | sed 's/^当前契约版本：//' | sort -u) || readme_vers=""
# CONTRACT 里是加粗写法「契约版本：**1.5.2**」，先去掉星号再取 X.Y.Z
contract_vers=$(grep '契约版本：' docs/CONTRACT.md 2>/dev/null | tr -d '*' \
  | grep -oE '契约版本：[0-9]+\.[0-9]+\.[0-9]+' | sed 's/^契约版本：//' | head -n 1) || contract_vers=""
e_bad=0
if [ -z "${readme_vers}" ]; then
  report_fail "README.md 中找不到「当前契约版本：X.Y.Z」锚点行。"
  e_bad=1
elif [ "$(printf '%s\n' "${readme_vers}" | wc -l | tr -d '[:space:]')" -ne 1 ]; then
  report_fail "README.md 中出现多个不同的当前契约版本：$(printf '%s' "${readme_vers}" | tr '\n' ' ')。"
  e_bad=1
elif ! printf '%s' "${readme_vers}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  report_fail "README.md 的「当前契约版本」不是 X.Y.Z 形式：「${readme_vers}」。"
  e_bad=1
fi
if [ -z "${contract_vers}" ]; then
  report_fail "docs/CONTRACT.md 中找不到「契约版本：X.Y.Z」（允许「契约版本：**X.Y.Z**」加粗写法）。"
  e_bad=1
fi
if [ "${e_bad}" -eq 0 ] && [ "${readme_vers}" != "${contract_vers}" ]; then
  report_fail "契约版本不同步：README.md 为 ${readme_vers}，docs/CONTRACT.md 为 ${contract_vers}。"
  e_bad=1
fi
if [ "${e_bad}" -eq 0 ]; then
  echo "  通过（契约版本：${readme_vers}）"
fi

# ---------- 汇总 ----------
if [ "${errors}" -gt 0 ]; then
  echo "" >&2
  echo "文档护栏：共 ${errors} 项失败（详见上方「失败」行），不通过。" >&2
  exit 1
fi
echo "文档护栏：检查 a/b/c/d/e 全部通过。"
