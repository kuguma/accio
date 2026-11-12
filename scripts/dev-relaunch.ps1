# 開発用: 稼働中の accio を止め、リリースビルドして昇格状態で再起動する。
# 非昇格のセッションからは `schtasks /Run /TN accio-dev` で起動する（タスクは最高権限で登録しておく）。
$log = "$env:LOCALAPPDATA\accio\dev-relaunch.log"
"start $(Get-Date -Format s)" | Out-File $log
taskkill /F /IM accio.exe 2>&1 | Out-File $log -Append
Start-Sleep 1
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$out = & cargo build --release -p accio 2>&1
$out | Select-Object -Last 3 | Out-File $log -Append
if ($LASTEXITCODE -eq 0) {
  Start-Process (Join-Path $root "target\release\accio.exe")
  "launched $(Get-Date -Format s)" | Out-File $log -Append
} else {
  "build failed" | Out-File $log -Append
}
