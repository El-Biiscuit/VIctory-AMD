[CmdletBinding()]
param([string]$Runtime = "$env:LOCALAPPDATA\DexNom Runtime")
$ErrorActionPreference = 'Stop'
$destination = Join-Path ([Environment]::GetFolderPath('MyDocuments')) ("VIctory-P0-" + [guid]::NewGuid().ToString('N'))
$files = 'victory.exe', 'victory-play.exe', 'victory-warm.exe'
foreach ($name in $files) {
    if (!(Test-Path -LiteralPath (Join-Path $Runtime $name) -PathType Leaf)) { throw "Absent : $name dans $Runtime" }
}
New-Item -ItemType Directory -Path $destination | Out-Null
$manifest = foreach ($name in $files) {
    $source = Join-Path $Runtime $name
    $copy = Join-Path $destination $name
    Copy-Item -LiteralPath $source -Destination $copy
    $hash = (Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash
    if ($hash -ne (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash) { throw "Copie invalide : $name" }
    [pscustomobject]@{ File = $name; SHA256 = $hash }
}
$manifest | Export-Csv -NoTypeInformation -Path (Join-Path $destination 'sha256.csv')
Write-Output $destination
