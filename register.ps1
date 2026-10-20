# Registers JSON Forge for .json / .jsonl files for the current user (no
# admin needed): the document icon, "Open with" entries, and a Default Apps
# listing. Run again with -Unregister to remove everything it added.
#
#   powershell -ExecutionPolicy Bypass -File register.ps1
#   powershell -ExecutionPolicy Bypass -File register.ps1 -Unregister
#
# Windows keeps the user's own "always open with" choice in a hashed key that
# scripts cannot set. If .json files still open elsewhere afterwards, pick
# JSON Forge once via right-click > Open with > Choose another app > Always.

param([switch]$Unregister)

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "json-forge.exe"
$progId = "JsonForge.Document"
$classes = "HKCU:\Software\Classes"
$appKey = "$classes\Applications\json-forge.exe"
$capRoot = "HKCU:\Software\JSON Forge"
$exts = @(".json", ".jsonl")

function Set-Default($key, $value) {
    if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
    Set-ItemProperty -Path $key -Name "(default)" -Value $value
}
function Set-Value($key, $name, $value) {
    if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
    Set-ItemProperty -Path $key -Name $name -Value $value
}
function Get-Default($key) {
    if (Test-Path $key) { (Get-Item $key).GetValue("") } else { $null }
}

if ($Unregister) {
    # Give each extension back whatever it pointed at before.
    foreach ($ext in $exts) {
        $prev = (Get-ItemProperty "$capRoot\Previous" -Name $ext -ErrorAction SilentlyContinue).$ext
        if ((Get-Default "$classes\$ext") -eq $progId) {
            if ($prev) { Set-Default "$classes\$ext" $prev }
            else { Remove-ItemProperty "$classes\$ext" -Name "(default)" -ErrorAction SilentlyContinue }
        }
        Remove-ItemProperty "$classes\$ext\OpenWithProgids" -Name $progId -ErrorAction SilentlyContinue
    }
    Remove-Item "$classes\$progId" -Recurse -ErrorAction SilentlyContinue
    Remove-Item "$appKey\DefaultIcon" -Recurse -ErrorAction SilentlyContinue
    Remove-Item $capRoot -Recurse -ErrorAction SilentlyContinue
    Remove-ItemProperty "HKCU:\Software\RegisteredApplications" -Name "JSON Forge" -ErrorAction SilentlyContinue
    Write-Host "JSON Forge unregistered."
} else {
    if (-not (Test-Path $exe)) { throw "json-forge.exe not found next to this script: $exe" }
    $docIcon = "`"$exe`",-2"   # resource id 2 = document icon (see build.rs)
    $appIcon = "`"$exe`",-1"
    $open = "`"$exe`" `"%1`""

    # The file type.
    Set-Default "$classes\$progId" "JSON Document"
    Set-Value "$classes\$progId" "FriendlyTypeName" "JSON Document"
    Set-Default "$classes\$progId\DefaultIcon" $docIcon
    Set-Default "$classes\$progId\shell\open\command" $open

    # Extensions: remember the previous handler so -Unregister can restore it.
    foreach ($ext in $exts) {
        $prev = Get-Default "$classes\$ext"
        if ($prev -and $prev -ne $progId) { Set-Value "$capRoot\Previous" $ext $prev }
        Set-Default "$classes\$ext" $progId
        Set-Value "$classes\$ext\OpenWithProgids" $progId ""
    }
    Set-Value "$classes\.json" "Content Type" "application/json"
    Set-Value "$classes\.json" "PerceivedType" "text"
    Set-Value "$classes\.jsonl" "PerceivedType" "text"

    # The application as Windows lists it in "Open with"; its DefaultIcon is
    # what Explorer draws on .json files when the user picked it there.
    Set-Value $appKey "FriendlyAppName" "JSON Forge"
    Set-Default "$appKey\DefaultIcon" $docIcon
    Set-Default "$appKey\shell\open\command" $open
    foreach ($ext in $exts) { Set-Value "$appKey\SupportedTypes" $ext "" }

    # Default Apps listing (Settings > Apps > Default apps).
    Set-Value "$capRoot\Capabilities" "ApplicationName" "JSON Forge"
    Set-Value "$capRoot\Capabilities" "ApplicationDescription" "Viewer and editor for JSON documents"
    Set-Value "$capRoot\Capabilities" "ApplicationIcon" $appIcon
    foreach ($ext in $exts) { Set-Value "$capRoot\Capabilities\FileAssociations" $ext $progId }
    Set-Value "HKCU:\Software\RegisteredApplications" "JSON Forge" "Software\JSON Forge\Capabilities"
    Write-Host "JSON Forge registered for $($exts -join ', ') -> $exe"
}

# Tell Explorer the associations changed so icons refresh without a logoff.
Add-Type -Namespace Win32 -Name Shell -MemberDefinition @"
[DllImport("shell32.dll")] public static extern void SHChangeNotify(int eventId, uint flags, IntPtr item1, IntPtr item2);
"@
[Win32.Shell]::SHChangeNotify(0x08000000, 0x0000, [IntPtr]::Zero, [IntPtr]::Zero)
