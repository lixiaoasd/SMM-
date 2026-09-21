#!/bin/bash
# 全量同步（后台运行，断点续传，日志 sync-all.log）
# flock 防止与每日定时任务并发
cd /opt/mirror
nohup /usr/bin/flock -n /tmp/mirror-sync.lock \
    /usr/bin/python3 /opt/mirror/sync-nexus.py \
    --period 1m --workers 6 --proxy http://127.0.0.1:7890 \
    > /opt/mirror/sync-all.log 2>&1 &
echo "started PID=$!"
sleep 6
echo "=== log head ==="
head -8 /opt/mirror/sync-all.log
