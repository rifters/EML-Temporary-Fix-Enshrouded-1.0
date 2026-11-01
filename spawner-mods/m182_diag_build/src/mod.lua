local TAG = "[m182-build-test]"

local function try(f)
    local ok, v = pcall(f)
    if ok then return v end
    return nil
end

local config = try(function() return require("m182_diag_build_config") end) or {}
if config.build_anywhere == false then
    print(TAG .. " build_anywhere is off, nothing changed")
    return
end

local items = try(function() return game.assets.get_resources_by_type("keen::ItemInfo") end)
local tools, changed = 0, 0
if type(items) == "table" then
    for _, resource in ipairs(items) do
        pcall(function()
            local equipment = resource.data.equipment
            if tostring(equipment.slot) ~= "BuildTool" then return end
            tools = tools + 1
            if equipment.buildZoneRequired == true then
                equipment.buildZoneRequired = false
                if equipment.buildZoneRequired == false then changed = changed + 1 end
            end
        end)
    end
end
print(string.format("%s build tools: %d, no longer need a build zone: %d", TAG, tools, changed))
