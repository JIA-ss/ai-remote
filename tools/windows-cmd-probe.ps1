# Diagnose redirected cmd output without changing the machine's locale or console settings.
$ErrorActionPreference = 'Stop'
$script = 'echo "a & b" | findstr /c:"a & b" && echo 世界 100%'
$cmd = Join-Path $env:SystemRoot 'System32\cmd.exe'
$variants = @{
    nested = ('/v:on /d /s /c "chcp 65001 >nul & "{0}" /v:off /d /s /c !AI_REMOTE_CMD_SCRIPT!"' -f $cmd)
    unicode = ('/d /u /s /c "{0}"' -f $script)
}
foreach ($name in $variants.Keys) {
    $info = [Diagnostics.ProcessStartInfo]::new($cmd, $variants[$name])
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.Environment['AI_REMOTE_CMD_SCRIPT'] = '"' + $script + '"'
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    [void]$process.Start()
    $bytes = [IO.MemoryStream]::new()
    $output = $process.StandardOutput.BaseStream.CopyToAsync($bytes)
    $errorOutput = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(10000)) { $process.Kill($true); throw 'cmd probe timed out' }
    $output.GetAwaiter().GetResult()
    @{variant=$name; exit_code=$process.ExitCode; utf8=[Text.Encoding]::UTF8.GetString($bytes.ToArray()); raw_base64=[Convert]::ToBase64String($bytes.ToArray()); stderr=$errorOutput.GetAwaiter().GetResult()} | ConvertTo-Json -Compress
    $process.Dispose()
}
