#!/bin/bash
# 星露谷镜像站（smmsever.xyz）源站初始化 —— Ubuntu 24.04
#
# 用法：把本目录（mirror-server/）里这几个文件一起传到服务器，然后
#       sudo bash setup-server.sh
#
# ============================================================
# 执行前必须先在 Cloudflare 控制台做三件事（脚本无法代做）：
#
#   ① DNS：添加 A 记录 smmsever.xyz -> <源站公网 IP>，代理状态 = Proxied（橙云）
#   ② 证书：SSL/TLS -> Origin Server -> Create Certificate
#        主机名填：smmsever.xyz 和 *.smmsever.xyz
#        有效期选 15 年；把返回的「源证书」「私钥」存到源站：
#          /etc/nginx/ssl/smmsever.xyz.pem    （证书，含中间证书）
#          /etc/nginx/ssl/smmsever.xyz.key    （私钥）
#          chmod 600 /etc/nginx/ssl/smmsever.xyz.key
#        提示：手动粘贴时别漏了 BEGIN/END 行，也别把私钥和证书写反。
#   ③ 加密模式：SSL/TLS -> Overview -> 选 Full (strict)，并打开 Always Use HTTPS
#        （Origin CA 证书只被 Cloudflare 信任，所以必须 strict，不能用 Flexible）
#
# 顺带建议（都在面板里点）：
#   - Cache Rules：/mods/* 与 /thumbs/* 用 Cache Everything + Edge TTL 1 天，
#     900 个模组 4GB 命中边缘缓存后源站几乎不出带宽
#   - 阿里云安全组：只放 22（限自己 IP）与 443，删掉 80
# ============================================================
set -euo pipefail

MIRROR_ROOT=/var/www/mirror
SSL_DIR=/etc/nginx/ssl
SITE=/etc/nginx/sites-available/mirror
SYNC_USER=admin          # 每日同步服务(mirror-sync.service)运行的用户
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '\n=== %s ===\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
    echo "请用 root 执行：sudo bash setup-server.sh" >&2
    exit 1
fi

# ---------- 前置检查：缺证书/缺文件就整体退出，绝不动正在服务的 nginx ----------
say "前置检查"
missing=0
for f in nginx-mirror.conf update-cloudflare-ips.sh update-cloudflare-ips.service update-cloudflare-ips.timer; do
    if [ ! -f "$DIR/$f" ]; then
        echo "缺少文件：$DIR/$f（请把 mirror-server/ 整个目录传上来）" >&2
        missing=1
    fi
done
for f in "$SSL_DIR/smmsever.xyz.pem" "$SSL_DIR/smmsever.xyz.key"; do
    if [ ! -f "$f" ]; then
        echo "缺少证书：$f —— 见本脚本顶部「执行前必须先在 Cloudflare 做三件事」第 ② 步" >&2
        missing=1
    fi
done
[ "$missing" -eq 0 ] || { echo "检查未通过，未做任何改动。" >&2; exit 1; }
echo "文件与证书齐全 ✓"

# ---------- 装包 ----------
say "安装 nginx / ufw / fail2ban / curl"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx ufw fail2ban curl >/dev/null
nginx -v

# ---------- Cloudflare IP 列表（必须先于站点配置生效，否则 $is_cloudflare 未定义） ----------
say "生成 Cloudflare 回源 IP 列表"
install -m 755 "$DIR/update-cloudflare-ips.sh" /usr/local/bin/update-cloudflare-ips.sh
install -m 644 "$DIR/update-cloudflare-ips.service" /etc/systemd/system/update-cloudflare-ips.service
install -m 644 "$DIR/update-cloudflare-ips.timer" /etc/systemd/system/update-cloudflare-ips.timer
/usr/local/bin/update-cloudflare-ips.sh || {
    echo "IP 列表生成失败（服务器能否访问 cloudflare.com？），先解决再重跑。" >&2
    exit 1
}

# ---------- 站点配置（旧配置先备份，出问题可回滚） ----------
say "安装站点配置"
BACKUP=/etc/nginx/backup-$(date +%Y%m%d-%H%M%S)
mkdir -p "$BACKUP"
[ -f "$SITE" ] && cp -a "$SITE" "$BACKUP/"
[ -L /etc/nginx/sites-enabled/default ] && mv /etc/nginx/sites-enabled/default "$BACKUP/default"
[ -f /etc/nginx/sites-enabled/default ] && mv /etc/nginx/sites-enabled/default "$BACKUP/default"

install -m 644 "$DIR/nginx-mirror.conf" "$SITE"
ln -sfn "$SITE" /etc/nginx/sites-enabled/mirror

# 静态目录：已存在就一律不动（避免改坏同步脚本的写入权限）
if [ ! -d "$MIRROR_ROOT" ]; then
    if id "$SYNC_USER" >/dev/null 2>&1; then
        install -d -o "$SYNC_USER" -g "$SYNC_USER" "$MIRROR_ROOT"
    else
        install -d "$MIRROR_ROOT"
    fi
    echo "已创建 $MIRROR_ROOT（空目录，等同步任务写入）"
fi

say "校验并加载 nginx"
if ! nginx -t; then
    echo "" >&2
    echo "nginx 配置校验失败：正在运行的 nginx 未受影响，站点仍是旧配置。" >&2
    echo "把上面的报错贴给我，或回滚：cp -a $BACKUP/. /etc/nginx/ 后 systemctl reload nginx" >&2
    exit 1
fi
systemctl enable nginx >/dev/null 2>&1 || true
systemctl reload nginx 2>/dev/null || systemctl restart nginx

# ---------- 防火墙 ----------
say "配置 ufw（22 + 443 放行，80 拒绝）"
ufw allow OpenSSH >/dev/null 2>&1 || ufw allow 22/tcp >/dev/null
ufw allow 443/tcp >/dev/null
ufw deny 80/tcp >/dev/null
ufw --force enable >/dev/null
ufw status verbose | sed -n '1,12p'

# ---------- fail2ban ----------
say "配置 fail2ban（sshd）"
cat > /etc/fail2ban/jail.d/sshd.local <<'EOF'
[sshd]
enabled = true
backend = systemd
maxretry = 5
findtime = 10m
bantime = 1h
EOF
systemctl enable fail2ban >/dev/null 2>&1 || true
systemctl restart fail2ban
sleep 1
fail2ban-client status sshd | sed -n '1,6p' || true

# ---------- 定时刷新 CF IP ----------
say "启用 Cloudflare IP 每日刷新"
systemctl daemon-reload
systemctl enable --now update-cloudflare-ips.timer >/dev/null 2>&1
systemctl list-timers update-cloudflare-ips.timer --no-pager | sed -n '1,3p'

# ---------- 自检 ----------
say "自检"
if sudo -u www-data test -r "$MIRROR_ROOT/index.json"; then
    echo "静态目录可读 ✓（www-data 能读到 index.json）"
else
    echo "⚠ www-data 读不到 $MIRROR_ROOT/index.json —— 检查目录/文件权限（目录需要 o+rx）" >&2
fi
echo "源站本机直连测试（预期：被 Cloudflare 白名单拦掉，返回 444 或空响应）"
curl -s -o /dev/null -w '  127.0.0.1 -> HTTP %{http_code}\n' -k \
    --resolve smmsever.xyz:443:127.0.0.1 https://smmsever.xyz/index.json || true

cat <<'EOF'

=== 接下来做什么 ===
1. 在你自己电脑上验证（走 Cloudflare）：
     curl -I https://smmsever.xyz/index.json
     curl -I https://smmsever.xyz/mods/<挑一个最大的 zip>
   第二个命令重点看：能不能正常下完一个 >100MB 的模组（免费版对上传限 100MB，
   下载方向无官方上限，但这是整套方案唯一需要实测的地方）。
2. 阿里云安全组：放行 443，删除 80；22 建议只留你自己的 IP。
3. 发布带新镜像地址的客户端版本，等用户更新一段时间后再确认 80 已彻底关闭。
   （80 现在返回 444，旧版客户端 http://<IP> 会直接失败——这是你选的策略）

=== 回滚 ===
   站点/防火墙/证书都没动同步脚本，回滚只要：
     cp -a <上面的 BACKUP 目录>/. /etc/nginx/ && systemctl reload nginx
     ufw allow 80/tcp && ufw delete deny 80/tcp
EOF