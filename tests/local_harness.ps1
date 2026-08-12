[CmdletBinding()]
param(
    [ValidateRange(1, 9)]
    [int]$Samples = 1,
    [switch]$SkipBuild,
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$targetTriple = "x86_64-pc-windows-msvc"
$targetDir = Join-Path $repoRoot "target/local-measure"
$resultsDir = Join-Path $repoRoot "target/local-measure-results"
$binary = Join-Path $targetDir "$targetTriple/release/clsp.exe"
$releaseDeps = Join-Path $targetDir "$targetTriple/release/deps"
$jsonPath = Join-Path $resultsDir "measurement.json"
$textPath = Join-Path $resultsDir "measurement.txt"

function Invoke-Captured {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(Mandatory)] [string[]]$Arguments
    )

    $output = @(& $FilePath @Arguments 2>&1 | ForEach-Object { [string]$_ })
    [pscustomobject]@{
        exit_code = [int]$LASTEXITCODE
        output = $output
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(Mandatory)] [string[]]$Arguments
    )

    $result = Invoke-Captured -FilePath $FilePath -Arguments $Arguments
    if ($result.exit_code -ne 0) {
        $tail = (@($result.output | Select-Object -Last 20) -join [Environment]::NewLine).Trim()
        throw "$FilePath $($Arguments -join ' ') failed with exit code $($result.exit_code)`n$tail"
    }
    $result.output
}

function Invoke-Measured {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(Mandatory)] [string[]]$Arguments
    )

    $watch = [Diagnostics.Stopwatch]::StartNew()
    $result = Invoke-Captured -FilePath $FilePath -Arguments $Arguments
    $watch.Stop()
    if ($result.exit_code -ne 0) {
        $tail = (@($result.output | Select-Object -Last 20) -join [Environment]::NewLine).Trim()
        throw "$FilePath $($Arguments -join ' ') failed with exit code $($result.exit_code)`n$tail"
    }
    [pscustomobject]@{
        milliseconds = [math]::Round($watch.Elapsed.TotalMilliseconds, 3)
        output = $result.output
    }
}

function Add-Sample {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [double]$Milliseconds
    )

    if (-not $script:metricSamples.ContainsKey($Name)) {
        $script:metricSamples[$Name] = [System.Collections.Generic.List[double]]::new()
    }
    $script:metricSamples[$Name].Add([math]::Round($Milliseconds, 3))
}

function Get-Median {
    param([Parameter(Mandatory)] [double[]]$Values)

    $sorted = @($Values | Sort-Object)
    $middle = [int][math]::Floor($sorted.Count / 2.0)
    if ($sorted.Count % 2 -eq 1) {
        return [double]$sorted[$middle]
    }
    return ([double]$sorted[$middle - 1] + [double]$sorted[$middle]) / 2
}

function Measure-CargoTest {
    param(
        [Parameter(Mandatory)] [string]$Metric,
        [Parameter(Mandatory)] [string]$Filter
    )

    for ($sample = 0; $sample -lt $Samples; $sample++) {
        $result = Invoke-Measured -FilePath "cargo" -Arguments @(
            "test", "--locked", "--lib", $Filter, "--target", $targetTriple,
            "--target-dir", $targetDir
        )
        Add-Sample -Name $Metric -Milliseconds $result.milliseconds
    }
}

New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null

$metadataText = (Invoke-Checked -FilePath "cargo" -Arguments @(
    "metadata", "--locked", "--no-deps", "--format-version", "1"
)) -join [Environment]::NewLine
$metadata = $metadataText | ConvertFrom-Json
$package = @($metadata.packages | Where-Object { $_.name -eq "clsp" })
if ($package.Count -ne 1) {
    throw "Expected one clsp package in Cargo metadata"
}
if ([string]$package[0].rust_version -ne "1.97.1") {
    throw "Cargo metadata reports rust-version '$($package[0].rust_version)', expected 1.97.1"
}

$rustcVersion = ((Invoke-Checked -FilePath "rustc" -Arguments @("--version")) -join " ").Trim()
$cargoVersion = ((Invoke-Checked -FilePath "cargo" -Arguments @("--version")) -join " ").Trim()
$lockHash = (Get-FileHash -LiteralPath (Join-Path $repoRoot "Cargo.lock") -Algorithm SHA256).Hash.ToLowerInvariant()
$metricSamples = @{}
$buildCacheState = if (Test-Path -LiteralPath $releaseDeps -PathType Container) { "warm" } else { "cold" }

if (-not $SkipBuild) {
    for ($sample = 0; $sample -lt $Samples; $sample++) {
        Invoke-Checked -FilePath "cargo" -Arguments @(
            "clean", "--package", "clsp", "--release", "--target-dir", $targetDir
        ) | Out-Null
        $build = Invoke-Measured -FilePath "cargo" -Arguments @(
            "build", "--release", "--locked", "--target", $targetTriple, "--target-dir", $targetDir
        )
        Add-Sample -Name "clean_release_build_ms" -Milliseconds $build.milliseconds
    }
} elseif (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "-SkipBuild was requested but '$binary' does not exist"
} else {
    $buildCacheState = "prebuilt"
}

if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "Release executable was not produced at '$binary'"
}

$versionOutput = ((Invoke-Checked -FilePath $binary -Arguments @("--version")) -join [Environment]::NewLine).Trim()
if ($versionOutput -ne "clsp $($package[0].version)") {
    throw "Release executable reports '$versionOutput', expected 'clsp $($package[0].version)'"
}

for ($sample = 0; $sample -lt $Samples; $sample++) {
    $startup = Invoke-Measured -FilePath $binary -Arguments @("--version")
    Add-Sample -Name "cli_startup_ms" -Milliseconds $startup.milliseconds
}

# Compile test targets once so the probe and full-suite samples measure runtime
# plus harness startup, not an accidental first compile.
Invoke-Checked -FilePath "cargo" -Arguments @(
    "test", "--locked", "--all-targets", "--no-run", "--target", $targetTriple,
    "--target-dir", $targetDir
) | Out-Null

# These are deterministic, fixture-only contract probes. They intentionally
# use Cargo's existing unit targets until the black-box local harness owns the
# fake LSP process; the metric names make that boundary explicit in reports.
Measure-CargoTest -Metric "broker_dispatch_probe_ms" -Filter "lease_lifecycle_is_visible_through_broker_interface"
Measure-CargoTest -Metric "lsp_query_position_probe_ms" -Filter "converts_unicode_positions_for_all_encodings"
Measure-CargoTest -Metric "lsp_diagnostics_probe_ms" -Filter "diagnostic_freshness_requires_matching_version"
Measure-CargoTest -Metric "ide_diagnostics_probe_ms" -Filter "ide_problems_baseline_reports_only_new_errors"
Measure-CargoTest -Metric "installer_probe_ms" -Filter "manager_probe_skips_failures_without_reordering"

if (-not $SkipTests) {
    for ($sample = 0; $sample -lt $Samples; $sample++) {
        $tests = Invoke-Measured -FilePath "cargo" -Arguments @(
            "test", "--all-targets", "--locked", "--target", $targetTriple,
            "--target-dir", $targetDir
        )
        Add-Sample -Name "full_locked_tests_ms" -Milliseconds $tests.milliseconds
    }
}

$treeOutput = (Invoke-Checked -FilePath "cargo" -Arguments @(
    "tree", "--locked", "--edges", "normal,build", "--target", $targetTriple
)) -join [Environment]::NewLine
$testOnlyDependency = @("tempfile", "assert_cmd", "predicates") | Where-Object {
    $escaped = [regex]::Escape($_)
    $treeOutput -match "(?m)(^|[^A-Za-z0-9_-])$escaped v[0-9]"
}
if ($testOnlyDependency.Count -gt 0) {
    throw "Test-only dependencies entered the release graph: $($testOnlyDependency -join ', ')"
}

$binaryText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($binary))
$markers = @(
    "tempfile",
    "test_broker",
    "diagnostic_freshness_requires_matching_version",
    "tests/unit",
    "#[cfg(test)]"
)
$foundMarkers = @($markers | Where-Object { $binaryText.Contains($_) })
if ($foundMarkers.Count -gt 0) {
    throw "Sampled test markers found in release binary: $($foundMarkers -join ', ')"
}

$metricResults = [ordered]@{}
foreach ($entry in ($metricSamples.GetEnumerator() | Sort-Object Name)) {
    $values = @($entry.Value | ForEach-Object { [math]::Round([double]$_, 3) })
    $metricResults[$entry.Key] = [ordered]@{
        samples_ms = $values
        median_ms = [math]::Round((Get-Median -Values ([double[]]$values)), 3)
    }
}

$result = [ordered]@{
    schema = 1
    revision = ((Invoke-Checked -FilePath "git" -Arguments @("rev-parse", "HEAD")) -join "").Trim()
    toolchain = [ordered]@{
        rustc = $rustcVersion
        cargo = $cargoVersion
        target = $targetTriple
        lockfile_sha256 = $lockHash
    }
    package = [ordered]@{
        name = [string]$package[0].name
        version = [string]$package[0].version
        rust_version = [string]$package[0].rust_version
    }
    artifact = [ordered]@{
        path = $binary
        bytes = (Get-Item -LiteralPath $binary).Length
        size_limit_bytes = 6291456
        test_markers_absent = ($foundMarkers.Count -eq 0)
        test_only_dependencies_absent = ($testOnlyDependency.Count -eq 0)
    }
    build = [ordered]@{
        clean_command = "cargo clean --package clsp --release --target-dir <dedicated-target>"
        cache_state = $buildCacheState
    }
    metrics = $metricResults
}

if ([int64]$result.artifact.bytes -gt [int64]$result.artifact.size_limit_bytes) {
    throw "Release executable is $($result.artifact.bytes) bytes; limit is $($result.artifact.size_limit_bytes) bytes"
}

$json = $result | ConvertTo-Json -Depth 8
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText($jsonPath, $json + [Environment]::NewLine, $utf8)

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add("CLSP local measurement (schema $($result.schema))")
$lines.Add("revision: $($result.revision)")
$lines.Add("rustc: $rustcVersion")
$lines.Add("target: $targetTriple")
$lines.Add("artifact: $($result.artifact.bytes) bytes (limit $($result.artifact.size_limit_bytes))")
foreach ($entry in $metricResults.GetEnumerator()) {
    $samplesText = (@($entry.Value.samples_ms) -join ", ")
    $lines.Add("$($entry.Key): [$samplesText] median=$($entry.Value.median_ms) ms")
}
$lines.Add("test-only dependencies absent: $($result.artifact.test_only_dependencies_absent)")
$lines.Add("sampled test markers absent: $($result.artifact.test_markers_absent)")
$lines.Add("json: $jsonPath")
[IO.File]::WriteAllLines($textPath, $lines, $utf8)
$lines | ForEach-Object { Write-Output $_ }
