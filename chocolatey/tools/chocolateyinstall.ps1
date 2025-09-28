$ErrorActionPreference = 'Stop';
$toolsDir = "$(Split-Path -parent $MyInvocation.MyCommand.Definition)"

$packageName = $env:ChocolateyPackageName
$version = $env:ChocolateyPackageVersion
$url64 = "https://github.com/OmneDAO/omne-cli/releases/download/v$version/omne-windows-x86_64.zip"

$packageArgs = @{
  packageName   = $packageName
  unzipLocation = $toolsDir
  fileType      = 'ZIP'
  url64bit      = $url64
  softwareName  = 'OMNE CLI*'
  checksum64    = 'PLACEHOLDER_CHECKSUM'
  checksumType64= 'sha256'
  validExitCodes= @(0)
}

Install-ChocolateyZipPackage @packageArgs

# Add to PATH
$exePath = Join-Path $toolsDir 'omne.exe'
Install-ChocolateyPath $toolsDir 'Machine'