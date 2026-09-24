#!/bin/bash
# 星露谷镜像站源站加固 —— 阶段一（IP 直连，域名还在 ICP 备案）
#
# 用法：把 mirror-server/ 里的 nginx-mirror.conf + 本脚本传到服务器，然后
#       sudo bash setup-server.sh
#
# 做四件事：
#   ① nginx 站点换成加固版（限流/限并发/隐藏版本/点文件禁读/缓存头）
#   ② ufw：只放 22 与 80
#   ③ fail2ban：sshd 防爆破
#   ④ 自检
#
# 不改动：/var/www/mirror 的属主与内容、同步脚本与 timer、任何已有证书
# 失败保护：nginx -t 不通过就整体退出，正在服务的旧配置继续生效；旧配置有备份
#
# 域名上线（阶段二，等备案通过）见脚本末尾提示，不在这里做。
set -euo pipefail

MIRROR_ROOT=/var/www/mirror
SITE=/etc/nginx/sites-available/mirror
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '\n=== %s ===\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
    echo "请用 root 执行：sudo bash setup-server.sh" >&2
    exit 1
fi

say "前置检查"
if [ ! -f "$DIR/nginx-mirror.conf" ]; then
    echo "缺少 $DIR/nginx-mirror.conf（请把 mirror-server/ 里的文件一起传上来）" >&2
    exit 1
fi
echo "站点配置就位 ✓"

say "安装 nginx / ufw / fail2ban"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx ufw fail2ban curl >/dev/null
nginx -v

say "安装站点配置（旧的先备份）"
BACKUP=/etc/nginx/backup-$(date +%Y%m%d-%H%M%S)
mkdir -p "$BACKUP"
[ -f "$SITE" ] && cp -a "$SITE" "$BACKUP/"
# default 站点也监听 80 default_server，会把我们的配置顶掉，先挪走
[ -e /etc/nginx/sites-enabled/default ] && mv /etc/nginx/sites-enabled/default "$BACKUP/default"

install -m 644 "$DIR/nginx-mirror.conf" "$SITE"
ln -sfn "$SITE" /etc/nginx/sites-enabled/mirror

# 静态目录已存在就一律不动（避免改坏同步脚本的写入权限）
[ -d "$MIRROR_ROOT" ] || install -d "$MIRROR_ROOT"

say "校验并加载 nginx"
if ! nginx -t; then
    echo "" >&2
    echo "nginx 校验失败：正在运行的 nginx 未受影响，仍是旧配置。" >&2
    echo "回滚：cp -a $BACKUP/. /etc/nginx/ && systemctl reload nginx" >&2
    exit 1
fi
systemctl enable nginx >/dev/null 2>&1 || true
systemctl reload nginx 2>/dev/null || systemctl restart nginx

say "配置 ufw（22 + 80）"
ufw allow OpenSSH >/dev/null 2>&1 || ufw allow 22/tcp >/dev/null
ufw allow 80/tcp >/dev/null
ufw --force enable >/dev/null
ufw status verbose | sed -n '1,10p'

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
fail2ban-client status sshd 2>/dev/null | sed -n '1,6p' || echo "（fail2ban 状态未取到，稍后 systemctl status fail2ban 看看）"

say "自检"
if sudo -u www-data test -r "$MIRROR_ROOT/index.json"; then
    echo "静态目录可读 ✓"
else
    echo "⚠ www-data 读不到 $MIRROR_ROOT/index.json —— 检查目录/文件权限（目录需要 o+rx）" >&2
fi
curl -s -o /dev/null -w '  本机 http://127.0.0.1/index.json -> HTTP %{http_code}\n' \
    -H "Host: 116.62.231.162" http://127.0.0.1/index.json || true
echo "  限流生效测试（连打 80 次，应能看到部分 503）："
for i in $(seq 1 80); do
    curl -s -o /dev/null -w '%{http_code}\n' -H "Host: 116.62.231.162" http://127.0.0.1/index.json
done | sort | uniq -c | sed 's/^/    /'

cat <<'EOF'

=== 已完成 ===
源站现在是：nginx 80 静态服务 + 流量加固（限流 30r/s、单 IP 16 连接、
隐藏版本号、点文件禁读、缓存头）+ ufw(22/80) + fail2ban(sshd)。
客户端继续用 http://116.62.231.162，不受影响。

=== 接下来：ICP 备案（域名才能在大陆服务器上对外服务）===
1. 阿里云控制台 → 备案 → 开始备案（个人）：
   - 需要：smmsever.xyz（已在阿里云注册且完成实名）、这台轻量服务器实例、
           备案服务码（轻量服务器控制台申请）、身份证、手机号、人脸核验
   - 网站名称/服务内容按实际填（例如「星露谷模组下载」）
   - 时长：阿里云初审 1~2 个工作日 → 管局审批，个人站一般 1~20 个工作日
   - 备案期间不要把域名解析到本机对外提供服务，会被阿里云阻断
2. 通过后：域名 A 记录 → 116.62.231.162（建议灰云直连，大陆用户更快）
3. 上 HTTPS：
     apt install -y certbot python3-certbot-nginx
     certbot --nginx -d smmsever.xyz
4. 然后把客户端默认镜像地址从 http://116.62.231.162 换成 https://smmsever.xyz，重新发版

=== 回滚 ===
    cp -a <上面的 BACKUP 目录>/. /etc/nginx/ && systemctl reload nginx
    ufw delete allow 80/tcp
EOF