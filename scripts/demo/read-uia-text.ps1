param(
    [Parameter(Mandatory=$true)][string]$Path,
    [string]$RuntimeId = "",
    [Parameter(Mandatory=$true)][string]$OutputPath
)

function Write-Result($obj) {
    $json = $obj | ConvertTo-Json -Compress
    $temporaryOutputPath = "$OutputPath.tmp"
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($temporaryOutputPath, $json, $encoding)
    if (Test-Path $OutputPath) {
        Remove-Item $OutputPath -Force
    }
    [System.IO.File]::Move($temporaryOutputPath, $OutputPath)
}

trap {
    Write-Result ([pscustomobject]@{ ok = $false; error = $_.Exception.Message })
    exit 1
}

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Find-ElementByPath([string]$ElementPath) {
    if ($ElementPath -notmatch '^0(/[0-9]+)*$') { throw "invalid element path" }
    $parts = $ElementPath -split '/'
    $current = [System.Windows.Automation.AutomationElement]::RootElement
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    for ($partIndex = 1; $partIndex -lt $parts.Count; $partIndex++) {
        $wanted = [int]$parts[$partIndex]
        $child = $walker.GetFirstChild($current)
        for ($index = 0; $index -lt $wanted; $index++) {
            if ($null -eq $child) { throw "element path no longer exists" }
            $child = $walker.GetNextSibling($child)
        }
        if ($null -eq $child) { throw "element path no longer exists" }
        $current = $child
    }
    return $current
}

$element = Find-ElementByPath $Path
if ($RuntimeId) {
    if ($RuntimeId -notmatch '^-?[0-9]+(\.-?[0-9]+)*$') { throw "invalid runtime id" }
    $actual = ""
    try { $actual = ($element.GetRuntimeId() -join ".") } catch { $actual = "" }
    if (-not $actual) { throw "element runtime id unavailable for comparison" }
    if ($actual -ne $RuntimeId) { throw "element runtime id changed" }
}

$text = $null
$method = ""
$valuePattern = $null
try { $valuePattern = $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern) } catch { $valuePattern = $null }
if ($null -ne $valuePattern) {
    $text = $valuePattern.Current.Value
    $method = "ValuePattern"
}

if ($null -eq $text) {
    $textPattern = $null
    try { $textPattern = $element.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern) } catch { $textPattern = $null }
    if ($null -ne $textPattern) {
        $text = $textPattern.DocumentRange.GetText(-1)
        $method = "TextPattern"
    }
}

if ($null -eq $text) {
    throw "element exposes neither ValuePattern nor TextPattern text"
}

$textBase64 = [System.Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($text))
Write-Result ([pscustomobject]@{
    ok = $true
    method = $method
    textBase64 = $textBase64
})
exit 0
