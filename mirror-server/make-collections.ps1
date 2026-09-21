# Build public/collections.json — 10 curated "hot collections" (cross-category
# virtual tags) from public/index.json. Each collection = explicit curated
# modId seeds (newly synced headliners + library mods) + keyword expansion over
# independent (non-translation-pack) mods. Re-run after every index rebuild.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$indexPath = Join-Path $root 'public\index.json'
$outPath   = Join-Path $root 'public\collections.json'

$parsed = [IO.File]::ReadAllText($indexPath) | ConvertFrom-Json
$index = @($parsed)
$byId = @{}
foreach ($e in $index) { $byId[[int64]$e.mod_id] = $e }

# 汉化/外语翻译包不参与关键词扩入（精确种子不受此限）。
$transRe = '(?i)translat|tradu|spolszczen|çeviri|i18n|汉化|漢化|中文|简体|繁[體体]|chinese|taiwanese|french|fran[cç]ai|portugu|spanish|russian|\brus\b|bulgarian|korean|\bkor\b|vietnamese|german|deutsch|japanese|italian|polish|turkish|thai|hungarian|czech|ukrainian|greek|dutch|swedish|norwegian|danish|finnish|한국|日本語|\besp\b|pt-?br|zh-?(tw|cn)'

# 合集定义：seeds = 人工精选 modId；kw = 对 name+description+category_en 做的正则。
$defs = @(
    [ordered]@{
        id='newbie'; zh='新手必装'; en='Starter Essentials'
        desc='第一次装模组？从这些开始：必备前置框架 + 最常用便利功能'
        seeds=@(1915,1348,5098,6529,3231,1720,4970,1726,
                541,239,518,1691,7098,8000,6183,21264,1089,3109,1845,
                859,7409,279,7500,7495,533,5,19675)
        kw=''
    },
    [ordered]@{
        id='automation'; zh='懒人自动化'; en='Automation & Lazy Farming'
        desc='自动浇水收割、拖拉机、祝尼魔打工，彻底解放双手'
        seeds=@(1063,1401,17767,7920,2731,1601,3109,2221,7920)
        kw='automat|tractor|sprinkler|\bjunimo|grabber|scythe|auto-?gate|winter grass|harvest(ing)? machine|self-?water|auto-?pet|auto-?collect|unlockable bundle'
    },
    [ordered]@{
        id='portraits'; zh='美化与肖像'; en='Visuals & Portraits'
        desc='高清肖像、建筑材质、季节外观，给鹈鹕镇换一套新皮肤'
        seeds=@(5969,5450,4542,4736)
        kw='portrait|recolou?r|retexture|seasonal (japanese )?buildings?|\bbuildings?\b|tilesheet|anime|hairstyle|hair style|\bhair\b|appearance|vibrant|pastoral|aesthetic|visual overhaul|\bvisuals?\b|map redesign|facelift'
    },
    [ordered]@{
        id='npcs'; zh='新NPC与恋爱'; en='New NPCs & Romance'
        desc='新增可婚角色、恋爱事件与海量 NPC 对话扩展'
        seeds=@(2544,5371,8626,7089)
        kw='\bnpcs?\b|new character|custom npc|romanc|marriag|bachelor|bachelorette|villager|dialogue|companion|new resident|new people'
    },
    [ordered]@{
        id='fishing'; zh='钓鱼专精'; en='Fishing Specialists'
        desc='鱼情透视、抛竿信息与鱼类可视化，钓鱼佬专属强化'
        seeds=@(2697,8970,8897,5735)
        kw='(?<![a-z])fish|angling|\bbait\b|tackle|trout|crab ?pot'
    },
    [ordered]@{
        id='combat'; zh='魔法战斗矿洞'; en='Magic, Combat & Mines'
        desc='法术体系、深林秘境、骷髅洞电梯与怪物战斗扩展'
        seeds=@(2007,2571,963)
        kw='magic|spell|combat|\bfight|battle|enem|monster|(?<![a-z])min(e|es|ing)\b|\bmines\b|cavern|skull|weapon|sword|deep ?woods|dungeon|\bslime'
    },
    [ordered]@{
        id='furniture'; zh='服装家具装饰'; en='Fashion, Furniture & Decor'
        desc='时装系统、动画家具与房屋内饰改造，装修换装两不误'
        seeds=@(9969,5662,4542,5969,5450)
        kw='fashion|cloth|outfit|wardrobe|dress|furniture|decor|wallpaper|flooring|\binterior|lamp|cushion|cottage'
    },
    [ordered]@{
        id='farm'; zh='农场农业畜牧'; en='Farming, Crops & Animals'
        desc='新农场地图、作物畜牧、温室洒水，经营你的大庄园'
        seeds=@(3231,4970,1063,1401,859,1601,17767,2221)
        kw='(?<![a-z])farm|crop|agricultur|animal|ranching|livestock|\bbarn\b|\bcoop\b|greenhouse|fruit tree|orchard|poultry|tractor|sprinkler|hay|bee ?house|\bsilo\b|fertiliz'
    },
    [ordered]@{
        id='cheats'; zh='作弊休闲'; en='Cheats & Casual'
        desc='CJB 作弊菜单、物品生成与各种跳过，休闲玩家随心所欲'
        seeds=@(4,93,2697,963,533)
        kw='cheat|spawner|noclip|god mode|infinite|unlimited|instant|\bskip\b|free ?love|friendship decay|no decay|warps? (menu|to)'
    },
    [ordered]@{
        id='expansion'; zh='大型扩展DLC'; en='Large Expansions / DLC'
        desc='SVE、里村、东斯卡普、祖祖城等大型内容扩展，相当于免费 DLC'
        seeds=@(3753,7286,5787,8587,6372,10342,9247)
        kw='expanded|expansion|ridgeside|east scarp|zuzu|aquarium|overhaul|new (region|area|town|village|map|island|location)|large-?scale|big content|mega ?mod|total conversion|new lands?'
    }
)

$result = @()
foreach ($d in $defs) {
    $ids = New-Object 'System.Collections.Generic.HashSet[int64]'
    # 1) 精确种子（库里有才收录；绕过翻译包过滤）。
    foreach ($sid in $d.seeds) {
        if ($byId.ContainsKey([int64]$sid)) { [void]$ids.Add([int64]$sid) }
    }
    # 2) 关键词扩入（剔除翻译包）。
    if ($d.kw) {
        foreach ($e in $index) {
            $name = [string]$e.name
            if ($name -match $transRe) { continue }
            $txt = "$name $([string]$e.description) $([string]$e.category_en)"
            if ($txt -match $d.kw) { [void]$ids.Add([int64]$e.mod_id) }
        }
    }
    # 按人气（_downloads）降序输出。
    $mods = @($ids | ForEach-Object { $byId[$_] } |
        Sort-Object @{Expression = { [int64]$_._downloads }; Descending = $true} |
        ForEach-Object { [int64]$_.mod_id })
    $result += [ordered]@{
        id    = $d.id
        zh    = $d.zh
        en    = $d.en
        desc  = $d.desc
        mods  = $mods
    }
    Write-Host ("{0,-11} {1,-12} {2,4} mods" -f $d.id, $d.zh, $mods.Count)
}

$json = ConvertTo-Json -InputObject $result -Depth 5
[IO.File]::WriteAllText($outPath, $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "wrote $outPath"
