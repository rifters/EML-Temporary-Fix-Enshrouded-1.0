local loaded, DATA = false, nil
if io.exists("src/m182_data.lua") == true then
    loaded, DATA = pcall(require, "m182_data")
end
if not loaded or type(DATA) ~= "table" then DATA = {} end
local OVERRIDES = type(DATA.overrides) == "table" and DATA.overrides or {}
local LEVELS = type(DATA.levels) == "table" and DATA.levels or {}
local BALANCE = type(DATA.balance) == "table" and DATA.balance or {}
for _, o in pairs(OVERRIDES) do
    if type(o) == "table" then
        if type(o.dmg) == "table" then
            local named = {}
            for i, key in ipairs({"speedMod","dmgMod","physicalCutDamage","physicalPierceDamage","physicalBluntDamage","magicalFireDamage","magicalIceDamage","magicalFogDamage","magicalLightningDamage","healing","poisonDamage","woodDamage","stoneDamage","metalDamage","explosionDamage"}) do if o.dmg[i] ~= nil then named[key] = o.dmg[i] end end
            o.dmg = named
        end
        if type(o.arm) == "table" then
            local named = {}
            for i, key in ipairs({"physical","blunt","pierce","cut","magical","fire","ice","fog","lightning"}) do if o.arm[i] ~= nil then named[key] = o.arm[i] end end
            o.arm = named
        end
    end
end
local FREE_SKILLS = BALANCE["freeSkills"] == 1
BALANCE["freeSkills"] = nil
local STACK = tonumber(BALANCE["stackFactor"]) or 1
local ALTAR = tonumber(BALANCE["altarFactor"]) or 1
BALANCE["stackFactor"] = nil
BALANCE["altarFactor"] = nil
if STACK < 1 then STACK = 1 end
if ALTAR < 1 then ALTAR = 1 end
local WIDEN = tonumber(BALANCE["itemLevelCap"]) or 0
if WIDEN <= 55 then WIDEN = 0 end
print(string.format("[m182-item-stats] data: itemLevelCap=%s playerLevelCap=%s stackFactor=%s altarFactor=%s", tostring(BALANCE["itemLevelCap"]), tostring(BALANCE["playerLevelCap"]), tostring(STACK), tostring(ALTAR)))
if next(OVERRIDES) == nil and next(LEVELS) == nil and next(BALANCE) == nil and not FREE_SKILLS and WIDEN == 0 and STACK == 1 and ALTAR == 1 then
    print("[m182-item-stats] no item settings saved, nothing to apply")
    do return end
end

local function try(f)
    local ok, r = pcall(f)
    if ok then return r end
    return nil
end

local function hash_of(v)
    if v == nil then return nil end
    local t = type(v)
    if t == "number" then return v end
    if t == "table" then return v.value or v.id or v.hash end
    local inner = try(function() return v.value end)
    if type(inner) == "number" then return inner end
    return tonumber(tostring(v))
end

local INTEGER_KEYS = {
    durabilityLossCap = true, playerBaseHealth = true, playerHealthPerAP = true, playerBaseMana = true,
    playerManaPerAP = true, playerBaseStamina = true, playerStaminaPerAP = true, itemLevelCap = true,
    playerLevelCap = true, hookShotCostBase = true, maximumAnimalsInWorld = true,
    woodDamage = true, stoneDamage = true, metalDamage = true, explosionDamage = true,
}

local function put(container, key, value)
    if type(value) ~= "number" or value ~= value or value == math.huge or value == -math.huge then return false end
    if INTEGER_KEYS[key] then
        value = math.floor(value + 0.5)
        if value < 0 then value = 0 end
    end
    return pcall(function() container[key] = value end)
end

local applied, failed = 0, 0
print("[m182-item-stats] start")

local function join(t, n)
    local out = {}
    for i = 1, n do out[i] = tostring(t[i]) end
    return table.concat(out, ",")
end

local function unsigned(id)
    if type(id) ~= "number" then return nil end
    id = math.floor(id)
    if id < 0 then id = id + 4294967296 end
    return id
end

local function close(a, b)
    if type(a) ~= "number" or type(b) ~= "number" then return false end
    return math.abs(a - b) <= 0.001 + math.abs(b) * 0.0001
end

local function write_group(container, values, label, report)
    if container == nil then
        failed = failed + 1
        report.missing = report.missing + 1
        return
    end
    for k, v in pairs(values) do
        local target = container
        if label == "dmg" and k ~= "speedMod" and k ~= "dmgMod" then target = try(function() return container.distribution end) end
        if label == "arm" then target = try(function() return container.distribution end) end
        if target ~= nil and put(target, k, v) then
            applied = applied + 1
            report.written = report.written + 1
            local back = try(function() return target[k] end)
            local expected = v
            if INTEGER_KEYS[k] then expected = math.floor(v + 0.5) end
            if not close(back, expected) then
                report.mismatch = report.mismatch + 1
                if report.first == nil then report.first = string.format("%s=%s expected %s", k, tostring(back), tostring(expected)) end
            end
        else
            failed = failed + 1
            report.missing = report.missing + 1
        end
    end
end

local found = {}
local levelsDone = {}
local widened = 0
local stacked = 0
local items = try(function() return game.assets.get_resources_by_type("keen::ItemInfo") end)
if items ~= nil then
    pcall(function()
        for _, resource in ipairs(items) do
            pcall(function()
                local data = resource.data
                local id = unsigned(hash_of(data.itemId))
                local range = id and LEVELS[id]
                if STACK > 1 then
                    local ms = try(function() return data.maxStackSize end)
                    if type(ms) == "number" and ms > 1 then
                        local want = math.min(65535, math.floor(ms * STACK + 0.5))
                        if want > ms and put(data, "maxStackSize", want) then stacked = stacked + 1 end
                    end
                end
                if WIDEN > 0 then
                    local wspan = try(function() return data.itemLevelRange end)
                    local wlo = try(function() return wspan.minLevel end)
                    local whi = try(function() return wspan.maxLevel end)
                    if type(wlo) == "number" and type(whi) == "number" and whi > wlo and whi < WIDEN and put(wspan, "maxLevel", WIDEN) then
                        widened = widened + 1
                    end
                end
                if range ~= nil then
                    local span = try(function() return data.itemLevelRange end)
                    local lo = try(function() return span.minLevel end)
                    local hi = try(function() return span.maxLevel end)
                    local wrote = false
                    if span ~= nil and type(lo) == "number" and type(hi) == "number" then
                        local newLo, newHi = math.min(lo, range[1]), math.max(hi, range[2])
                        if newLo ~= lo then wrote = put(span, "minLevel", newLo) end
                        if newHi ~= hi then wrote = put(span, "maxLevel", newHi) end
                        print(string.format("[m182-item-stats] item %d level range %s-%s -> %s-%s (%s)", id, tostring(lo), tostring(hi),
                            tostring(try(function() return span.minLevel end)), tostring(try(function() return span.maxLevel end)), wrote and "written" or "unchanged"))
                    else
                        print(string.format("[m182-item-stats] item %d has no readable level range", id))
                    end
                    levelsDone[id] = true
                end
                local o = id and OVERRIDES[id]
                if o == nil then return end
                local report = { written = 0, mismatch = 0, missing = 0 }
                if o.dmg ~= nil then write_group(try(function() return data.damageSetup end), o.dmg, "dmg", report) end
                if o.arm ~= nil then write_group(try(function() return data.armorSetup end), o.arm, "arm", report) end
                found[id] = (found[id] or 0) + 1
                print(string.format("[m182-item-stats] item %d (%s): wrote %d, not written %d, read-back mismatches %d%s",
                    id, tostring(try(function() return data.debugName end)), report.written, report.missing, report.mismatch,
                    report.first and (" first: " .. report.first) or ""))
            end)
        end
    end)
end
for id in pairs(OVERRIDES) do
    if found[id] == nil then print(string.format("[m182-item-stats] item %d not found in the game data", id)) end
end
for id in pairs(LEVELS) do
    if levelsDone[id] == nil then print(string.format("[m182-item-stats] item %d (level range) not found in the game data", id)) end
end

print(string.format("[m182-item-stats] items done applied=%d failed=%d widened=%d stacked=%d", applied, failed, widened, stacked))

if FREE_SKILLS then
    local nodes, freed = 0, 0
    local trees = try(function() return game.assets.get_resources_by_type("keen::SkillTreeResource") end)
    if type(trees) == "table" then
        for _, tree in ipairs(trees) do
            pcall(function()
                for _, node in ipairs(tree.data.nodes) do
                    nodes = nodes + 1
                    if put(node, "costs", 0) then freed = freed + 1 end
                end
            end)
        end
    end
    print(string.format("[m182-item-stats] skills free of cost: %d of %d nodes", freed, nodes))
end

local tables = try(function() return game.assets.get_resources_by_type("keen::BalancingTable") end)
if type(tables) == "table" and #tables > 0 then
    local d = tables[1].data
    if ALTAR > 1 then
        local list = try(function() return d.altarsPerFlameLevel end)
        local before, after = {}, {}
        if list ~= nil then
            local count = 0
            for i, v in ipairs(list) do
                count = count + 1
                before[i] = v
                if type(v) == "number" and v > 0 then
                    put(list, i, math.min(255, math.floor(v * ALTAR + 0.5)))
                end
                after[i] = try(function() return list[i] end)
            end
            print(string.format("[m182-item-stats] altars per flame level: %d entries, before {%s} after {%s}", count, join(before, count), join(after, count)))
        else
            print("[m182-item-stats] altars per flame level: field not found")
        end
    end
    for key, value in pairs(BALANCE) do
        local rarity = string.match(key, "^rarityDurabilityMod%.(%a+)$")
        local ok
        if rarity then
            ok = put(d.rarityDurabilityMod, rarity, value)
        else
            ok = put(d, key, value)
        end
        if ok then applied = applied + 1 else failed = failed + 1 end
        local back = try(function()
            if rarity then return d.rarityDurabilityMod[rarity] end
            return d[key]
        end)
        print(string.format("[m182-item-stats] balance %s = %s (%s)", key, tostring(back), ok and "written" or "not written"))
    end
end

print(string.format("[m182-item-stats] values applied=%d failed=%d", applied, failed))

