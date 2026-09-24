#!/bin/bash
# 生成 nginx 用的 Cloudflare 回源 IP 列表：/etc/nginx/conf.d/cloudflare-ips.conf
#
# 生成内容：
#   geo $realip_remote_addr $is_cloudflare { ... }   —— 判断请求是否来自 Cloudflare
#   set_real_ip_from ... / real_ip_header CF-Connecting-IP; —— 还原访客真实 IP，
#                                                             让限流按真实客户端算
# 由 update-cloudflare-ips.service/timer 每天执行；setup-server.sh 首次会先跑一遍。
#
# 注意：内容没变化就不 reload nginx；拉取失败时保留旧文件，绝不写空列表
#       （空列表 = 所有请求 $is_cloudflare 都是 0 = 全部被 444 丢弃）。
set -euo pipefail

OUT=/etc/nginx/conf.d/cloudflare-ips.conf
V4_URL=https://www.cloudflare.com/ips-v4
V6_URL=https://www.cloudflare.com/ips-v6

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

fetch() { curl -fsS --max-time 20 "$1"; }

{
    echo "# 由 update-cloudflare-ips.sh 自动生成，请勿手改。"
    echo "# 来源：$V4_URL 与 $V6_URL"
    echo "geo \$realip_remote_addr \$is_cloudflare {"
    echo "    default 0;"
    fetch "$V4_URL" | sed 's|^|    |; s|$| 1;|'
    fetch "$V6_URL" | sed 's|^|    |; s|$| 1;|'
    echo "}"
    echo
    fetch "$V4_URL" | sed 's|^|set_real_ip_from |; s|$|;|'
    fetch "$V6_URL" | sed 's|^|set_real_ip_from |; s|$|;|'
    echo "real_ip_header CF-Connecting-IP;"
} > "$TMP"

# 至少要有 10 条 CIDR，否则视为拉取异常
COUNT=$(grep -cE '^[[:space:]]*[0-9a-fA-F:.]+/[0-9]+ 1;$' "$TMP" || true)
if [ "$COUNT" -lt 10 ]; then
    echo "拉取失败或内容异常（只有 $COUNT 条），保留原文件" >&2
    exit 1
fi

if [ -f "$OUT" ] && cmp -s "$TMP" "$OUT"; then
    echo "Cloudflare IP 列表无变化（$COUNT 条）"
    exit 0
fi

install -m 644 "$TMP" "$OUT"
echo "已更新 $OUT（$COUNT 条）"

if nginx -t 2>/dev/null; then
    systemctl reload nginx
    echo "nginx 已 reload"
else
    echo "nginx 配置校验失败，请手动检查：nginx -t" >&2
    exit 1
fi