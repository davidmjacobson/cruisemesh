# Create and drive emulators for Google Play listing screenshots.
#
# Play Console wants 9:16 (or 16:9) PNGs, no alpha, and a clean status bar.
# Phone: 1080x1920. 7-inch: 1200x2133. 10-inch: 1600x2844. All 9:16.
# Stock Pixel 7 is 1080x2400 and exceeds Play's 2:1 max-side ratio.
#
# Usage (from the repo root):
#   .\tools\play_store_emulator.ps1 start
#   .\tools\play_store_emulator.ps1 start -Form tablet10
#   .\tools\play_store_emulator.ps1 seed -Form tablet10
#   .\tools\play_store_emulator.ps1 capture -Form tablet10 -Name chats
#   .\tools\play_store_emulator.ps1 stop -Form tablet10
#
# `start` creates the AVD if it is missing, boots it, and enters System UI
# demo mode. Screenshots land in tmp/play-store-screenshots/ (gitignored).

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet("setup", "start", "demo", "capture", "seed", "stop", "status")]
    [string]$Action = "start",

    [ValidateSet("phone", "tablet7", "tablet10")]
    [string]$Form = "phone",

    [string]$Name,

    [string]$OutDir
)

$ErrorActionPreference = "Stop"

$SystemImage = "system-images;android-35;google_apis;x86_64"

switch ($Form) {
    "phone" {
        $AvdName = "play-store-phone"
        $EmulatorPort = 5570
        # Generic phone, not pixel_7: Pixel 7's corner radius is sized for
        # 2400px and clips the nav bar when we pin the framebuffer to 1080x1920.
        $DeviceProfile = "medium_phone"
        $LcdWidth = 1080
        $LcdHeight = 1920
        $LcdDensity = 420
        $RamMb = 3072
    }
    "tablet7" {
        $AvdName = "play-store-tablet7"
        $EmulatorPort = 5572
        $DeviceProfile = "Nexus 7 2013"
        # 9:16 at ~7.6" and 600dp wide so Compose uses a tablet-sized layout.
        $LcdWidth = 1200
        $LcdHeight = 2133
        $LcdDensity = 320
        $RamMb = 2048
    }
    "tablet10" {
        $AvdName = "play-store-tablet10"
        $EmulatorPort = 5574
        $DeviceProfile = "Nexus 10"
        # 9:16 at ~10.2" and 800dp wide. 1600x2844 is 1600*16/9.
        $LcdWidth = 1600
        $LcdHeight = 2844
        $LcdDensity = 320
        $RamMb = 2048
    }
}

$Serial = "emulator-$EmulatorPort"

$Sdk = $env:ANDROID_SDK_ROOT
if (-not $Sdk) { $Sdk = $env:ANDROID_HOME }
if (-not $Sdk) { $Sdk = Join-Path $env:LOCALAPPDATA "Android\Sdk" }
if (-not (Test-Path -LiteralPath $Sdk)) {
    throw "Android SDK not found. Set ANDROID_HOME or install Android Studio."
}

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$Adb = Join-Path $Sdk "platform-tools\adb.exe"
$Emulator = Join-Path $Sdk "emulator\emulator.exe"
$AvdManager = Join-Path $Sdk "cmdline-tools\latest\bin\avdmanager.bat"
$SdkManager = Join-Path $Sdk "cmdline-tools\latest\bin\sdkmanager.bat"
$AvdHome = Join-Path $env:USERPROFILE ".android\avd"
$AvdDir = Join-Path $AvdHome "$AvdName.avd"
$AvdIni = Join-Path $AvdHome "$AvdName.ini"
$ConfigIni = Join-Path $AvdDir "config.ini"

if (-not $OutDir) {
    $OutDir = switch ($Form) {
        "tablet7" { Join-Path $RepoRoot "tmp\play-store-screenshots\listing-7in" }
        "tablet10" { Join-Path $RepoRoot "tmp\play-store-screenshots\listing-10in" }
        default { Join-Path $RepoRoot "tmp\play-store-screenshots" }
    }
}

function Assert-File([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "Required tool not found: $Path"
    }
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)]
        [string]$File,
        [string[]]$NativeArgs = @()
    )
    # adb/emulator write progress to stderr; Stop would treat that as fatal.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $File @NativeArgs 2>&1
        [pscustomobject]@{
            ExitCode = $LASTEXITCODE
            Lines    = @($output | ForEach-Object { "$_" })
        }
    } finally {
        $ErrorActionPreference = $prev
    }
}

function Invoke-Adb([string[]]$AdbArgs) {
    $result = Invoke-Native -File $Adb -NativeArgs (@("-s", $Serial) + $AdbArgs)
    if ($result.ExitCode -ne 0) {
        throw "adb -s $Serial $($AdbArgs -join ' ') failed (exit $($result.ExitCode))."
    }
    $result.Lines
}

function Test-EmulatorOnline {
    $result = Invoke-Native -File $Adb -NativeArgs @("devices")
    return [bool]($result.Lines | Where-Object { $_ -match "^$([regex]::Escape($Serial))\s+device$" })
}

function Wait-EmulatorBoot {
    param([int]$TimeoutSeconds = 240)

    Write-Host "Waiting for $Serial to appear on adb..."
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-EmulatorOnline) { break }
        Start-Sleep -Seconds 2
    }
    if (-not (Test-EmulatorOnline)) {
        throw "Emulator $Serial did not come online within ${TimeoutSeconds}s."
    }

    Write-Host "Waiting for Android to finish booting..."
    while ((Get-Date) -lt $deadline) {
        $result = Invoke-Native -File $Adb -NativeArgs @("-s", $Serial, "shell", "getprop", "sys.boot_completed")
        $boot = (($result.Lines | Select-Object -Last 1) -as [string]).Trim()
        if ($boot -eq "1") { break }
        Start-Sleep -Seconds 2
    }
    $result = Invoke-Native -File $Adb -NativeArgs @("-s", $Serial, "shell", "getprop", "sys.boot_completed")
    $boot = (($result.Lines | Select-Object -Last 1) -as [string]).Trim()
    if ($boot -ne "1") {
        throw "Emulator $Serial booted to adb but sys.boot_completed never flipped."
    }

    # Package manager and SystemUI come up a few seconds after boot_completed.
    Start-Sleep -Seconds 5
}

function Set-AvdConfig([string]$Path, [hashtable]$Values) {
    $lines = @()
    if (Test-Path -LiteralPath $Path) {
        $lines = [System.IO.File]::ReadAllLines($Path)
    }
    $seen = @{}
    $out = foreach ($line in $lines) {
        if ($line -match "^(?<k>[^=]+)=(.*)$" -and $Values.ContainsKey($Matches.k)) {
            $seen[$Matches.k] = $true
            "$($Matches.k)=$($Values[$Matches.k])"
        } else {
            $line
        }
    }
    foreach ($key in $Values.Keys) {
        if (-not $seen.ContainsKey($key)) {
            $out += "$key=$($Values[$key])"
        }
    }
    [System.IO.File]::WriteAllLines($Path, $out)
}

function Install-SystemImage {
    Assert-File $SdkManager
    $imageDir = Join-Path $Sdk ($SystemImage -replace ";", [IO.Path]::DirectorySeparatorChar)
    if (Test-Path -LiteralPath $imageDir) {
        Write-Host "System image already installed: $SystemImage"
        return
    }
    Write-Host "Installing $SystemImage (one-time download)..."
    $prev = $env:ANDROID_SDK_ROOT
    $env:ANDROID_SDK_ROOT = $Sdk
    try {
        & $SdkManager --install $SystemImage
        if ($LASTEXITCODE -ne 0) {
            throw "sdkmanager failed to install $SystemImage."
        }
    } finally {
        $env:ANDROID_SDK_ROOT = $prev
    }
}

function Install-PlayStoreAvd {
    Assert-File $AvdManager
    Install-SystemImage

    if (-not (Test-Path -LiteralPath $AvdHome)) {
        New-Item -ItemType Directory -Path $AvdHome | Out-Null
    }

    if (-not (Test-Path -LiteralPath $AvdIni)) {
        Write-Host "Creating AVD $AvdName ($DeviceProfile, $SystemImage)..."
        $prev = $env:ANDROID_SDK_ROOT
        $env:ANDROID_SDK_ROOT = $Sdk
        try {
            "no" | & $AvdManager create avd `
                --name $AvdName `
                --package $SystemImage `
                --device $DeviceProfile `
                --force
            if ($LASTEXITCODE -ne 0) {
                throw "avdmanager failed to create $AvdName."
            }
        } finally {
            $env:ANDROID_SDK_ROOT = $prev
        }
    } else {
        Write-Host "AVD $AvdName already exists; refreshing hardware config."
    }

    if (-not (Test-Path -LiteralPath $ConfigIni)) {
        throw "AVD created but $ConfigIni is missing."
    }

    # Pin the framebuffer to Play's recommended 9:16 phone size and turn the
    # GPU on. The Pixel 7 profile we cloned is 1080x2400 with GPU off.
    Set-AvdConfig $ConfigIni @{
        "hw.lcd.width"        = "$LcdWidth"
        "hw.lcd.height"       = "$LcdHeight"
        "hw.lcd.density"      = "$LcdDensity"
        "hw.gpu.enabled"      = "yes"
        "hw.gpu.mode"         = "host"
        "hw.keyboard"         = "yes"
        "hw.mainKeys"         = "no"
        "hw.ramSize"          = "$RamMb"
        "hw.cpu.ncore"        = "4"
        "showDeviceFrame"     = "no"
        "skin.path"           = "_no_skin"
        "skin.dynamic"        = "yes"
        "disk.dataPartition.size" = "8G"
    }

    Write-Host "AVD $AvdName ready at ${LcdWidth}x${LcdHeight} @ ${LcdDensity}dpi."
}

function Start-PlayStoreEmulator {
    Assert-File $Emulator
    Assert-File $Adb
    Install-PlayStoreAvd

    if (Test-EmulatorOnline) {
        Write-Host "$Serial is already running."
        Enable-DemoMode
        return
    }

    $portBusy = Get-NetTCPConnection -LocalPort $EmulatorPort -ErrorAction SilentlyContinue |
        Where-Object { $_.State -in @("Listen", "Established") }
    if ($portBusy) {
        throw "Port $EmulatorPort is in use. Stop the other emulator or change `$EmulatorPort."
    }

    Write-Host "Starting $AvdName on port $EmulatorPort..."
    $emuArgs = @(
        "-avd", $AvdName,
        "-port", "$EmulatorPort",
        "-gpu", "host",
        "-netdelay", "none",
        "-netspeed", "full",
        "-no-boot-anim"
    )
    Start-Process -FilePath $Emulator -ArgumentList $emuArgs -WorkingDirectory (Split-Path $Emulator) | Out-Null
    Wait-EmulatorBoot
    Enable-DemoMode
    Write-Host "Ready. Serial $Serial. Capture with: .\tools\play_store_emulator.ps1 capture -Name <slug>"
}

function Enable-DemoMode {
    if (-not (Test-EmulatorOnline)) {
        throw "$Serial is not running. Start it first."
    }

    Write-Host "Entering System UI demo mode (Play listing status bar)..."
    Invoke-Adb @("shell", "settings", "put", "global", "sysui_demo_allowed", "1") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "enter") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "clock", "-e", "hhmm", "0941") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "battery", "-e", "plugged", "false", "-e", "level", "100") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "network", "-e", "wifi", "show", "-e", "level", "4") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "network", "-e", "mobile", "show", "-e", "datatype", "4g", "-e", "level", "4") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "notifications", "-e", "visible", "false") | Out-Null
    Invoke-Adb @("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "status", "-e", "bluetooth", "hidden", "-e", "volume", "hidden", "-e", "mute", "hidden", "-e", "location", "hidden", "-e", "alarm", "hidden") | Out-Null
    Invoke-Adb @("shell", "settings", "put", "system", "show_touches", "0") | Out-Null
    Invoke-Adb @("shell", "settings", "put", "system", "pointer_location", "0") | Out-Null

    # Google APIs images default to 3-button nav. A Pixel 7 profile at
    # 1080x1920 still uses the taller device's corner radius, which clips
    # those buttons in half. Gesture nav plus hiding the bar matches a
    # current phone and keeps Play shots out of the clipped zone.
    Invoke-Adb @("shell", "settings", "put", "secure", "navigation_mode", "2") | Out-Null
    Invoke-Adb @("shell", "cmd", "overlay", "disable", "com.android.internal.systemui.navbar.threebutton") | Out-Null
    Invoke-Adb @("shell", "cmd", "overlay", "enable", "com.android.internal.systemui.navbar.gestural") | Out-Null
    Invoke-Adb @("shell", "settings", "put", "secure", "immersive_mode_confirmations", "confirmed") | Out-Null
    Invoke-Adb @("shell", "settings", "put", "global", "policy_control", "immersive.navigation=*") | Out-Null

    Write-Host "Demo mode on: 9:41, full battery/Wi-Fi/cell, no notifications, nav hidden."
}

function Capture-PlayStoreScreenshot {
    if (-not (Test-EmulatorOnline)) {
        throw "$Serial is not running. Start it first."
    }

    if (-not (Test-Path -LiteralPath $OutDir)) {
        New-Item -ItemType Directory -Path $OutDir | Out-Null
    }

    $stem = $Name
    if (-not $stem) {
        $stem = Get-Date -Format "yyyyMMdd-HHmmss"
    }
    $stem = ($stem -replace "[^A-Za-z0-9._-]", "-")
    $dest = Join-Path $OutDir "$stem.png"
    $remote = "/sdcard/Download/play-store-$stem.png"

    Invoke-Adb @("shell", "screencap", "-p", $remote) | Out-Null
    Invoke-Adb @("pull", $remote, $dest) | Out-Null
    Invoke-Adb @("shell", "rm", $remote) | Out-Null

    $bytes = [System.IO.File]::ReadAllBytes($dest)
    if ($bytes.Length -lt 24 -or $bytes[1] -ne 0x50 -or $bytes[2] -ne 0x4E -or $bytes[3] -ne 0x47) {
        throw "Pulled file is not a PNG: $dest"
    }
    $width = [BitConverter]::ToUInt32([byte[]]($bytes[19], $bytes[18], $bytes[17], $bytes[16]), 0)
    $height = [BitConverter]::ToUInt32([byte[]]($bytes[23], $bytes[22], $bytes[21], $bytes[20]), 0)
    if ($width -ne $LcdWidth -or $height -ne $LcdHeight) {
        Write-Warning "Screenshot is ${width}x${height}, expected ${LcdWidth}x${LcdHeight}."
    } else {
        Write-Host "Play-ready ${width}x${height} PNG."
    }
    Write-Host $dest
}

function Stop-PlayStoreEmulator {
    if (-not (Test-EmulatorOnline)) {
        Write-Host "$Serial is not running."
        return
    }
    Invoke-Adb @("emu", "kill") | Out-Null
    Write-Host "Stopped $Serial."
}

function Seed-PlayListing {
    if (-not (Test-EmulatorOnline)) {
        throw "$Serial is not running. Start it first."
    }
    Write-Host "Seeding Play listing inbox on $Serial..."
    Invoke-Adb @(
        "shell", "am", "broadcast",
        "-a", "com.cruisemesh.app.debug.SEED_PLAY_LISTING",
        "-n", "com.cruisemesh.app/com.cruisemesh.app.debug.DebugCommandReceiver"
    ) | Out-Null
    Invoke-Adb @("shell", "am", "force-stop", "com.cruisemesh.app") | Out-Null
    Start-Sleep -Seconds 1
    Invoke-Adb @(
        "shell", "am", "start",
        "-n", "com.cruisemesh.app/.MainActivity",
        "-a", "android.intent.action.MAIN",
        "-c", "android.intent.category.LAUNCHER"
    ) | Out-Null
    Enable-DemoMode
    Write-Host "Seed applied. App relaunched onto the chat list."
}

function Show-Status {
    Write-Host "SDK:      $Sdk"
    Write-Host "AVD:      $AvdName"
    Write-Host "Serial:   $Serial"
    Write-Host "Size:     ${LcdWidth}x${LcdHeight} @ ${LcdDensity}dpi"
    Write-Host "Image:    $SystemImage"
    Write-Host "AVD dir:  $(if (Test-Path -LiteralPath $AvdDir) { $AvdDir } else { '(not created)' })"
    Write-Host "Running:  $(if (Test-EmulatorOnline) { 'yes' } else { 'no' })"
    Write-Host "Out dir:  $OutDir"
    if (Test-Path -LiteralPath $ConfigIni) {
        Get-Content -LiteralPath $ConfigIni | Where-Object {
            $_ -match "^hw\.(lcd\.|gpu\.|keyboard|ramSize)" -or $_ -match "^(showDeviceFrame|skin\.path)="
        }
    }
}

switch ($Action) {
    "setup" { Install-PlayStoreAvd }
    "start" { Start-PlayStoreEmulator }
    "seed" { Seed-PlayListing }
    "demo" { Enable-DemoMode }
    "capture" { Capture-PlayStoreScreenshot }
    "stop" { Stop-PlayStoreEmulator }
    "status" { Show-Status }
}
