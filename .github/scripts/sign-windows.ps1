param(
  [Parameter(Mandatory = $true)]
  [string]$File
)

$ErrorActionPreference = "Stop"

if (-not $env:WINDOWS_CERTIFICATE) {
  throw "WINDOWS_CERTIFICATE is not set; cannot sign $File"
}
if (-not (Test-Path $File)) {
  throw "File to sign does not exist: $File"
}

$pfx = Join-Path $env:RUNNER_TEMP ("oaklet-sign-" + [guid]::NewGuid() + ".pfx")
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE))

try {
  $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1
  if (-not $signtool) {
    throw "signtool.exe was not found in the Windows SDK"
  }

  $timestamp = if ($env:WINDOWS_TIMESTAMP_URL) {
    $env:WINDOWS_TIMESTAMP_URL
  } else {
    "http://timestamp.digicert.com"
  }

  & $signtool.FullName sign /fd SHA256 /f $pfx /p $env:WINDOWS_CERTIFICATE_PASSWORD `
    /tr $timestamp /td SHA256 $File
  if ($LASTEXITCODE -ne 0) {
    throw "Signing failed for $File"
  }

  $signature = Get-AuthenticodeSignature -FilePath $File
  if ($signature.Status -ne "Valid") {
    throw "Authenticode verification failed for ${File}: $($signature.StatusMessage)"
  }
} finally {
  Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
