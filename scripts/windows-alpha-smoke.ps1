[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string] $PayloadDir,

  [string] $AppId = "1142710",

  [switch] $AllowMissingSteamRuntime
)

$ErrorActionPreference = "Stop"

function Resolve-RequiredFile {
  param(
    [Parameter(Mandatory = $true)]
    [string] $Path,

    [Parameter(Mandatory = $true)]
    [string] $Label
  )

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "$Label is missing at $Path"
  }

  return (Resolve-Path -LiteralPath $Path).Path
}

function Resolve-RequiredDirectory {
  param(
    [Parameter(Mandatory = $true)]
    [string] $Path,

    [Parameter(Mandatory = $true)]
    [string] $Label
  )

  if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
    throw "$Label is missing at $Path"
  }

  return (Resolve-Path -LiteralPath $Path).Path
}

$payload = Resolve-RequiredDirectory -Path $PayloadDir -Label "Windows payload directory"
$appExe = Resolve-RequiredFile -Path (Join-Path $payload "wh3mm-dioxus.exe") -Label "Dioxus app executable"
$helperDir = Resolve-RequiredDirectory -Path (Join-Path $payload "helpers") -Label "Steam helper directory"
$helperExe = Resolve-RequiredFile -Path (Join-Path $helperDir "wh3mm-steam-helper.exe") -Label "Steam helper executable"
$schemaFile = Resolve-RequiredFile -Path (Join-Path $payload "schema\schema_wh3.json.zst") -Label "WH3 compressed schema"
$helpFile = Resolve-RequiredFile -Path (Join-Path $payload "WINDOWS-ALPHA-README.md") -Label "Windows alpha verification guide"
$steamDll = Join-Path $helperDir "steam_api64.dll"

if (-not $AllowMissingSteamRuntime) {
  Resolve-RequiredFile -Path $steamDll -Label "Steam runtime DLL" | Out-Null
}

$fixturePath = Join-Path ([IO.Path]::GetTempPath()) ("wh3mm-steam-helper-smoke-{0}.json" -f [Guid]::NewGuid())
$commandLogPath = Join-Path ([IO.Path]::GetTempPath()) ("wh3mm-steam-helper-smoke-{0}.jsonl" -f [Guid]::NewGuid())

$oldBackend = $env:WH3MM_STEAM_HELPER_BACKEND
$oldFixture = $env:WH3MM_STEAM_HELPER_FIXTURE
$oldCommandLog = $env:WH3MM_STEAM_HELPER_COMMAND_LOG

function Set-Or-ClearEnv {
  param(
    [Parameter(Mandatory = $true)]
    [string] $Name,

    [AllowNull()]
    [string] $Value
  )

  if ($null -eq $Value) {
    Remove-Item -Path "Env:\$Name" -ErrorAction SilentlyContinue
  } else {
    Set-Item -Path "Env:\$Name" -Value $Value
  }
}

function Invoke-HelperJson {
  param(
    [Parameter(Mandatory = $true)]
    [string] $Command,

    [AllowNull()]
    [string] $Payload,

    [AllowNull()]
    [string] $DelayMs
  )

  $arguments = @($AppId, $Command)
  if ($null -ne $Payload) {
    $arguments += $Payload
  }
  if ($null -ne $DelayMs) {
    $arguments += $DelayMs
  }

  $output = & $helperExe @arguments
  if ($LASTEXITCODE -ne 0) {
    throw "Steam helper command '$Command' exited with code $LASTEXITCODE. Output: $output"
  }

  $jsonLine = $output | Where-Object { $_.Trim().Length -gt 0 } | Select-Object -Last 1
  if (-not $jsonLine) {
    throw "Steam helper command '$Command' did not print JSON."
  }

  $parsed = $jsonLine | ConvertFrom-Json
  return $parsed
}

try {
  $fixture = @{
    subscribedIds = @("111", "222")
    mods = @(
      @{
        publishedFileId = "111"
        title = "Smoke Fixture Mod"
        owner = @{ steamId64 = "76561198000000001" }
        timeUpdated = 1700000000
      }
    )
    items = @(
      @{
        publishedFileId = "333"
        title = "Smoke Dependency"
        owner = @{ steamId64 = "76561198000000002" }
        timeUpdated = 1700000001
      }
    )
    dependencies = @{ "111" = @("333") }
    authors = @{
      "76561198000000001" = "Smoke Author"
      "76561198000000002" = "Dependency Author"
    }
  } | ConvertTo-Json -Depth 8

  Set-Content -LiteralPath $fixturePath -Value $fixture -Encoding utf8

  $env:WH3MM_STEAM_HELPER_BACKEND = "fixture"
  $env:WH3MM_STEAM_HELPER_FIXTURE = $fixturePath
  $env:WH3MM_STEAM_HELPER_COMMAND_LOG = $commandLogPath

  $probe = Invoke-HelperJson -Command "probe"
  if ($probe.appId -ne $AppId) {
    throw "Probe appId mismatch: expected $AppId, got $($probe.appId)"
  }
  if ($probe.selectedBackend -ne "fixture") {
    throw "Probe selected backend mismatch: expected fixture, got $($probe.selectedBackend)"
  }
  if (-not $probe.fixtureAvailable) {
    throw "Probe did not report the fixture backend as available."
  }

  $runtimeStatus = @($probe.windowsRuntimeRedistributableStatuses) |
    Where-Object { $_.fileName -eq "steam_api64.dll" } |
    Select-Object -First 1
  if (-not $runtimeStatus) {
    throw "Probe did not include steam_api64.dll runtime status."
  }
  if (-not $AllowMissingSteamRuntime -and -not $runtimeStatus.present) {
    throw "Probe did not find steam_api64.dll beside the helper. Expected path: $($runtimeStatus.expectedPath)"
  }

  $subscribed = @(Invoke-HelperJson -Command "getSubscribedIds")
  if (($subscribed -join ",") -ne "111,222") {
    throw "Subscribed ID fixture response mismatch: $($subscribed -join ',')"
  }

  $metadata = Invoke-HelperJson -Command "getModsData" -Payload "111"
  $metadataMods = @($metadata.mods)
  if ($metadataMods.Count -ne 1 -or $metadataMods[0].publishedFileId -ne "111") {
    throw "getModsData fixture response did not include mod 111."
  }
  if ((@($metadata.dependencies."111") -join ",") -ne "333") {
    throw "getModsData fixture response did not include dependency 333 for mod 111."
  }

  $commandResult = Invoke-HelperJson -Command "checkState" -Payload "111;222;222;bad" -DelayMs "250"
  if ($commandResult.command -ne "checkState") {
    throw "checkState command response mismatch: $($commandResult.command)"
  }
  if ((@($commandResult.ids) -join ",") -ne "111,222") {
    throw "checkState ID normalization mismatch: $(@($commandResult.ids) -join ',')"
  }
  if ($commandResult.delayMs -ne 250) {
    throw "checkState delay mismatch: $($commandResult.delayMs)"
  }

  if (-not (Test-Path -LiteralPath $commandLogPath -PathType Leaf)) {
    throw "Steam helper command log was not written."
  }
  $commandLogCount = @(Get-Content -LiteralPath $commandLogPath).Count
  if ($commandLogCount -lt 4) {
    throw "Steam helper command log has too few entries: $commandLogCount"
  }

  Write-Host "Windows alpha payload smoke passed."
  Write-Host "App: $appExe"
  Write-Host "Helper: $helperExe"
  Write-Host "Schema: $schemaFile"
  Write-Host "Guide: $helpFile"
} finally {
  Set-Or-ClearEnv -Name "WH3MM_STEAM_HELPER_BACKEND" -Value $oldBackend
  Set-Or-ClearEnv -Name "WH3MM_STEAM_HELPER_FIXTURE" -Value $oldFixture
  Set-Or-ClearEnv -Name "WH3MM_STEAM_HELPER_COMMAND_LOG" -Value $oldCommandLog
  Remove-Item -LiteralPath $fixturePath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $commandLogPath -Force -ErrorAction SilentlyContinue
}
