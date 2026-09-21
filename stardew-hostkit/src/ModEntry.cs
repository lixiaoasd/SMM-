using System;
using System.Collections.Generic;
using System.IO;
using StardewModdingAPI;
using StardewModdingAPI.Events;
using StardewValley;

namespace FireSVM.HostKit
{
    /// <summary>管理器写入的配置（Mods\FireSVM.HostKit\hostkit-config.json）。</summary>
    public class HostKitConfig
    {
        /// <summary>启动时自动读取 SaveName 指定的存档并开房；消费一次后自动置回 false。</summary>
        public bool AutoHost;
        /// <summary>要开的存档文件夹名（%APPDATA%\StardewValley\Saves 下的目录名）。</summary>
        public string SaveName;
        /// <summary>房主无操作达到 AfkMinutes 分钟时自动暂停游戏时间。</summary>
        public bool AfkPause;
        public int AfkMinutes;
        /// <summary>没有其他玩家在线达到 EmptyMinutes 分钟时自动暂停（等人时不让农场白跑）。</summary>
        public bool EmptyPause;
        public int EmptyMinutes;
        /// <summary>玩家上下线公告。</summary>
        public bool AnnounceJoin;
        public bool AnnounceLeave;
        /// <summary>新玩家加入时的欢迎语；空串表示不发送。</summary>
        public string WelcomeText;
        /// <summary>手动冻结/恢复时间的热键名（SButton，如 F8）。</summary>
        public string FreezeHotkey;
        /// <summary>暂停/恢复时在游戏内聊天框提示。</summary>
        public bool NotifyPause;

        public void ApplyDefaults()
        {
            if (this.SaveName == null) { this.SaveName = ""; }
            if (this.WelcomeText == null) { this.WelcomeText = ""; }
            if (this.FreezeHotkey == null || this.FreezeHotkey.Length == 0) { this.FreezeHotkey = "F8"; }
            if (this.AfkMinutes <= 0) { this.AfkMinutes = 5; }
            if (this.EmptyMinutes <= 0) { this.EmptyMinutes = 10; }
        }
    }

    /// <summary>插件写给管理器的状态（Mods\FireSVM.HostKit\hostkit-status.json）。</summary>
    public class HostKitStatus
    {
        public bool Hosting;
        public bool TimePaused;
        /// <summary>暂停原因：afk / empty / manual；未暂停为空串。</summary>
        public string PausedBy;
        public string SaveName;
        public string PlayerName;
        public string FarmName;
        public int Players;
        public List<string> PlayerNames;
        public int TimeOfDay;
        public int SeasonIndex;
        public int DayOfMonth;
        public int Year;
        public int AfkSeconds;
        public int EmptySeconds;
        public string Note;
        public string LastEventTime;

        public HostKitStatus()
        {
            this.PausedBy = "";
            this.SaveName = "";
            this.PlayerName = "";
            this.FarmName = "";
            this.PlayerNames = new List<string>();
            this.Note = "";
            this.LastEventTime = "";
        }
    }

    /// <summary>广播给客户端的一句话（让所有玩家看到房主提示）。</summary>
    public class HostKitMessage
    {
        public string Text;
    }

    /// <summary>
    /// 房主助手：一键开服（自动读档 + 开房）、挂机自动暂停时间、
    /// 无人等待自动暂停、玩家上下线公告、热键/远程冻结时间。
    /// </summary>
    public class ModEntry : Mod
    {
        private const string ConfigFile = "hostkit-config.json";
        private const string StatusFile = "hostkit-status.json";
        private const string CmdFile = "hostkit-cmd.txt";
        private const string MessageType = "HostKitNotice";
        private const string ModID = "FireSVM.HostKit";

        private HostKitConfig Config = new HostKitConfig();
        private HostKitStatus Status = new HostKitStatus();

        /// <summary>房主最后一次操作的时刻。</summary>
        private DateTime lastActivity = DateTime.UtcNow;
        /// <summary>最近一次「没有其他玩家在线」的起始时刻。</summary>
        private DateTime emptySince = DateTime.UtcNow;

        private bool autoHostConsumed;
        private int titleTicks;
        private bool hosting;
        /// <summary>已经发出过读档请求（读档期间 IsWorldReady 仍为 false，避免重复触发）。</summary>
        private bool loadRequested;

        private string cmdPath = "";

        public override void Entry(IModHelper helper)
        {
            this.cmdPath = Path.Combine(helper.DirectoryPath, CmdFile);

            HostKitConfig loaded = helper.Data.ReadJsonFile<HostKitConfig>(ConfigFile);
            this.Config = loaded == null ? new HostKitConfig() : loaded;
            this.Config.ApplyDefaults();

            helper.Events.GameLoop.UpdateTicked += this.OnUpdateTicked;
            helper.Events.GameLoop.OneSecondUpdateTicked += this.OnOneSecond;
            helper.Events.GameLoop.SaveLoaded += this.OnSaveLoaded;
            helper.Events.GameLoop.ReturnedToTitle += this.OnReturnedToTitle;
            helper.Events.Input.ButtonsChanged += this.OnButtonsChanged;
            helper.Events.Input.CursorMoved += this.OnCursorMoved;
            helper.Events.Multiplayer.PeerConnected += this.OnPeerConnected;
            helper.Events.Multiplayer.PeerDisconnected += this.OnPeerDisconnected;
            helper.Events.Multiplayer.ModMessageReceived += this.OnModMessageReceived;

            this.Monitor.Log(
                "HostKit 已载入（自动开服=" + this.Config.AutoHost + "，存档=" + this.Config.SaveName + "）",
                LogLevel.Info);
            this.SaveStatus();
        }

        // ---------- 一键开服 ----------

        private void OnUpdateTicked(object sender, UpdateTickedEventArgs e)
        {
            if (this.autoHostConsumed || !this.Config.AutoHost) { return; }
            if (this.Config.SaveName == null || this.Config.SaveName.Length == 0) { return; }
            if (Context.IsWorldReady || this.loadRequested) { return; }

            // 等标题菜单稳定出现约 1 秒再动手，避免和启动初始化抢时序。
            if (Game1.activeClickableMenu is StardewValley.Menus.TitleMenu)
            {
                this.titleTicks++;
                if (this.titleTicks >= 60)
                {
                    this.autoHostConsumed = true;
                    this.StartHosting(this.Config.SaveName);
                }
            }
            else
            {
                this.titleTicks = 0;
            }
        }

        /// <summary>
        /// 自动读档并以房主身份开房，等价于联机菜单里点「主持」。
        /// 依据游戏原逻辑：multiplayerMode=2 + options.enableServer →
        /// 读档过程中 Multiplayer.updatePendingConnections 会自动 StartServer()。
        /// </summary>
        public void StartHosting(string saveName)
        {
            if (Context.IsWorldReady || this.loadRequested || saveName == null || saveName.Length == 0) { return; }
            try
            {
                Game1.options.enableServer = true;
                Game1.multiplayerMode = 2;
                SaveGame.Load(saveName);
                Game1.exitActiveMenu();
                this.loadRequested = true;
                this.Monitor.Log("已请求读档开房：" + saveName, LogLevel.Info);
                this.Status.Note = "正在读档开房：" + saveName;
                this.SaveStatus();
            }
            catch (Exception ex)
            {
                this.Monitor.Log("自动开服失败：" + ex.Message, LogLevel.Error);
                this.Status.Note = "自动开服失败：" + ex.Message;
                this.SaveStatus();
                return;
            }
            // 自动开服只生效一次：避免之后玩家正常启动游戏又被拽进存档。
            this.Config.AutoHost = false;
            this.Helper.Data.WriteJsonFile<HostKitConfig>(ConfigFile, this.Config);
        }

        // ---------- 存档与生命周期 ----------

        private void OnSaveLoaded(object sender, SaveLoadedEventArgs e)
        {
            this.loadRequested = false;
            this.hosting = Context.IsMultiplayer && Context.IsOnHostComputer;
            this.lastActivity = DateTime.UtcNow;
            this.emptySince = DateTime.UtcNow;
            this.Status.SaveName = StardewModdingAPI.Constants.SaveFolderName;
            if (Game1.player != null)
            {
                this.Status.PlayerName = Game1.player.Name;
                this.Status.FarmName = Game1.player.farmName.Value;
            }
            this.Status.Hosting = this.hosting;
            this.Status.Note = this.hosting ? "已开房，等待好友加入" : "已载入存档（未开房）";
            this.Monitor.Log("存档已载入：" + this.Status.SaveName + "，开房=" + this.hosting, LogLevel.Info);
            this.SaveStatus();
        }

        private void OnReturnedToTitle(object sender, ReturnedToTitleEventArgs e)
        {
            this.hosting = false;
            this.loadRequested = false;
            this.Status = new HostKitStatus();
            this.Status.Note = "已回到标题（服务器已关闭）";
            this.SaveStatus();
        }

        // ---------- AFK / 无人自动暂停 ----------

        private void OnButtonsChanged(object sender, ButtonsChangedEventArgs e)
        {
            this.MarkActivity();
        }

        private void OnCursorMoved(object sender, CursorMovedEventArgs e)
        {
            this.MarkActivity();
        }

        private void MarkActivity()
        {
            this.lastActivity = DateTime.UtcNow;
            if (this.Status.PausedBy == "afk")
            {
                this.SetTimePaused(false, "", "房主回到电脑前，时间继续");
            }
        }

        private void EvaluateAutoPause()
        {
            if (!this.hosting) { return; }

            double afk = (DateTime.UtcNow - this.lastActivity).TotalSeconds;
            this.Status.AfkSeconds = (int)afk;

            int remote = this.RemotePlayerCount();
            if (remote > 0)
            {
                this.emptySince = DateTime.UtcNow;
                if (this.Status.PausedBy == "empty")
                {
                    this.SetTimePaused(false, "", "有玩家加入，时间继续");
                }
            }
            else if (this.Status.PausedBy != "empty")
            {
                this.emptySince = DateTime.UtcNow;
            }
            this.Status.EmptySeconds = (int)(DateTime.UtcNow - this.emptySince).TotalSeconds;

            // 正在过节 / 正在播剧情 / 正在睡觉换日时不插手。
            if (Game1.CurrentEvent != null || Game1.isFestival() || Game1.newDay) { return; }
            if (this.Status.PausedBy == "manual") { return; }

            if (this.Config.AfkPause && this.Config.AfkMinutes > 0
                && afk >= this.Config.AfkMinutes * 60)
            {
                this.SetTimePaused(true, "afk",
                    "房主挂机 " + this.Config.AfkMinutes + " 分钟，时间已自动暂停");
                return;
            }
            if (this.Config.EmptyPause && this.Config.EmptyMinutes > 0
                && remote == 0 && this.Status.EmptySeconds >= this.Config.EmptyMinutes * 60)
            {
                this.SetTimePaused(true, "empty",
                    "无人在线 " + this.Config.EmptyMinutes + " 分钟，时间已自动暂停");
            }
        }

        /// <summary>除房主外的在线玩家数。</summary>
        private int RemotePlayerCount()
        {
            int total = 0;
            IEnumerable<Farmer> farmers = Game1.getOnlineFarmers();
            foreach (Farmer f in farmers) { total++; }
            return total <= 0 ? 0 : total - 1;
        }

        private void SetTimePaused(bool paused, string reason, string note)
        {
            if (Game1.netWorldState == null) { return; }
            bool current = Game1.netWorldState.Value.IsTimePaused;
            if (current == paused && this.Status.PausedBy == reason) { return; }
            Game1.netWorldState.Value.IsTimePaused = paused;
            this.Status.TimePaused = paused;
            this.Status.PausedBy = reason;
            this.Status.Note = note;
            this.Monitor.Log(note, LogLevel.Info);
            if (this.Config.NotifyPause)
            {
                this.Announce(paused ? "⏸ 游戏时间已暂停" : "▶ 游戏时间已恢复", false);
            }
            this.SaveStatus();
        }

        // ---------- 热键与远程指令 ----------

        private void OnOneSecond(object sender, OneSecondUpdateTickedEventArgs e)
        {
            // 指令在标题菜单也要收：管理器可能在游戏已经开着的情况下才点「开服」。
            this.PollCommand();
            if (Context.IsWorldReady)
            {
                this.hosting = Context.IsMultiplayer && Context.IsOnHostComputer;
            }
            if (this.hosting)
            {
                this.EvaluateAutoPause();
                this.RefreshStatus();
                this.SaveStatus();
            }
        }

        private void PollCommand()
        {
            string text = "";
            try
            {
                if (!File.Exists(this.cmdPath)) { return; }
                text = File.ReadAllText(this.cmdPath).Trim();
                if (text.Length == 0) { return; }
                File.WriteAllText(this.cmdPath, "");
            }
            catch (Exception ex)
            {
                this.Monitor.Log("读取指令失败：" + ex.Message, LogLevel.Warn);
                return;
            }

            // 管理器可能在游戏已经启动后才改设置，收到指令时顺手重读一次配置。
            HostKitConfig fresh = this.Helper.Data.ReadJsonFile<HostKitConfig>(ConfigFile);
            if (fresh != null)
            {
                fresh.ApplyDefaults();
                this.Config = fresh;
            }

            string[] parts = text.Split(new char[] { '|' }, 2);
            string cmd = parts[0].Trim().ToLowerInvariant();
            string arg = parts.Length > 1 ? parts[1] : "";

            if (cmd == "freeze")
            {
                this.SetTimePaused(true, "manual", "房主手动暂停了时间");
            }
            else if (cmd == "unfreeze")
            {
                this.SetTimePaused(false, "", "房主手动恢复了时间");
            }
            else if (cmd == "announce")
            {
                this.Announce(arg, true);
            }
            else if (cmd == "host")
            {
                // 只重新武装自动开服：真正的读档交给 OnUpdateTicked 等标题菜单稳定后触发，
                // 避免游戏刚启动、界面还没就绪时读档。
                if (arg != null && arg.Length > 0) { this.Config.SaveName = arg; }
                this.Config.AutoHost = true;
                this.autoHostConsumed = false;
                this.titleTicks = 0;
                this.Status.Note = "已收到开服指令，等待标题界面就绪…";
                this.SaveStatus();
                this.Monitor.Log("已收到开服指令：" + this.Config.SaveName, LogLevel.Info);
            }
            else if (cmd == "quit")
            {
                this.Status.Note = "正在保存并回到标题…";
                this.SaveStatus();
                Game1.ExitToTitle(null);
            }
        }

        private void RefreshStatus()
        {
            this.Status.Hosting = this.hosting;
            this.Status.TimePaused = Game1.netWorldState != null && Game1.netWorldState.Value.IsTimePaused;
            if (!this.Status.TimePaused) { this.Status.PausedBy = ""; }
            this.Status.TimeOfDay = Game1.timeOfDay;
            if (Game1.Date != null)
            {
                this.Status.SeasonIndex = Game1.Date.SeasonIndex;
                this.Status.DayOfMonth = Game1.Date.DayOfMonth;
                this.Status.Year = Game1.Date.Year;
            }
            List<string> names = new List<string>();
            int count = 0;
            IEnumerable<Farmer> farmers = Game1.getOnlineFarmers();
            foreach (Farmer f in farmers)
            {
                count++;
                if (f != null) { names.Add(f.Name); }
            }
            this.Status.Players = count;
            this.Status.PlayerNames = names;
        }

        // ---------- 玩家上下线与公告 ----------

        private void OnPeerConnected(object sender, PeerConnectedEventArgs e)
        {
            if (e.Peer != null && e.Peer.IsHost) { return; }
            if (this.Config.WelcomeText != null && this.Config.WelcomeText.Length > 0)
            {
                this.Announce(this.Config.WelcomeText, true);
            }
            else if (this.Config.AnnounceJoin)
            {
                this.Announce(" 有玩家加入了服务器", true);
            }
            this.Status.LastEventTime = DateTime.Now.ToString("HH:mm:ss");
            this.Status.Note = "有玩家加入";
            this.SaveStatus();
        }

        private void OnPeerDisconnected(object sender, PeerDisconnectedEventArgs e)
        {
            if (e.Peer != null && e.Peer.IsHost) { return; }
            if (this.Config.AnnounceLeave)
            {
                this.Announce("👋 有玩家离开了服务器", true);
            }
            this.Status.LastEventTime = DateTime.Now.ToString("HH:mm:ss");
            this.Status.Note = "有玩家离开";
            this.SaveStatus();
        }

        /// <summary>房主公告：本地聊天框显示 + 广播给所有装了 HostKit 的玩家。</summary>
        private void Announce(string text, bool broadcast)
        {
            if (text == null || text.Length == 0) { return; }
            if (Game1.chatBox != null)
            {
                Game1.chatBox.addInfoMessage("[房主] " + text);
            }
            if (!broadcast) { return; }
            try
            {
                HostKitMessage msg = new HostKitMessage();
                msg.Text = text;
                this.Helper.Multiplayer.SendMessage<HostKitMessage>(
                    msg, MessageType, new string[] { ModID }, null);
            }
            catch (Exception ex)
            {
                this.Monitor.Log("广播公告失败：" + ex.Message, LogLevel.Trace);
            }
        }

        /// <summary>客户端收到房主公告 → 显示在自己的聊天框。</summary>
        private void OnModMessageReceived(object sender, ModMessageReceivedEventArgs e)
        {
            if (e.FromModID != ModID || e.Type != MessageType) { return; }
            if (Context.IsOnHostComputer) { return; }
            HostKitMessage msg = e.ReadAs<HostKitMessage>();
            if (msg != null && msg.Text != null && Game1.chatBox != null)
            {
                Game1.chatBox.addInfoMessage("[房主] " + msg.Text);
            }
        }

        // ---------- 状态文件 ----------

        private void SaveStatus()
        {
            try
            {
                this.Helper.Data.WriteJsonFile<HostKitStatus>(StatusFile, this.Status);
            }
            catch (Exception ex)
            {
                this.Monitor.Log("写入状态文件失败：" + ex.Message, LogLevel.Trace);
            }
        }
    }
}