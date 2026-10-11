# FIX-5: testlerin Credential Manager'da biraktigi artik kayitlari temizler.
#
# Eski provider testleri gercek kasaya `dex-provider-test*` servis adiyla yaziyordu
# (ornegi `local.dex-provider-test-79532`); guncel testler kasaya hic dokunmaz
# (providers.rs `vault` mock, secrets.rs dosya fonksiyonlari). Bu betik yalnizca
# adinda `dex-provider-test` gecen hedefleri siler — gercek `*.dex-provider`
# kayitlari ile `*.dex-host` jetonlarina DOKUNMAZ.
#
# Kullanim: powershell -ExecutionPolicy Bypass -File scripts/clean-test-credentials.ps1

$ErrorActionPreference = "Stop"

$targets = cmd /c "cmdkey /list" 2>$1 | ForEach-Object {
  if ($_ -match "^\s*Target:\s*LegacyGeneric:target=(.+)\s*$") { $Matches[1].Trim() }
} | Where-Object { $_ -like "*dex-provider-test*" }

# Emniyet: gercek kayit deseniyle biten bir hedef asla listeye giremez; yine de cift kontrol.
$targets = @($targets | Where-Object { $_ -notmatch "\.dex-provider$" -and $_ -notmatch "\.dex-host$" })

if ($targets.Count -eq 0) {
  Write-Output "artik kayit yok (0 silindi)"
  exit 0
}

$deleted = 0
foreach ($t in $targets) {
  $out = cmd /c "cmdkey /delete:$t" 2>&1
  if ($LASTEXITCODE -eq 0) { $deleted++ } else { Write-Warning "silinemedi: $t ($out)" }
}
Write-Output "$deleted kayit silindi"
