# Disposable Windows Forms UIA fixture; no user application state is modified.
param([Parameter(Mandatory=$true)][string]$OutputPath)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object System.Windows.Forms.Form
$form.Text = 'ai-remote structured UI fixture'
$form.Size = New-Object System.Drawing.Size(480, 330)
$field = New-Object System.Windows.Forms.TextBox
$field.Name = 'fixture.input'; $field.AccessibleName = 'Input'
$field.Location = New-Object System.Drawing.Point(30,30); $field.Width = 360
$secure = New-Object System.Windows.Forms.TextBox
$secure.Name = 'fixture.password'; $secure.AccessibleName = 'Password'; $secure.UseSystemPasswordChar = $true
$secure.Text = 'fixture-secret-do-not-export'
$secure.Location = New-Object System.Drawing.Point(30,80); $secure.Width = 360
$button = New-Object System.Windows.Forms.Button
$button.Name = 'fixture.apply'; $button.Text = 'Apply'; $button.AccessibleName = 'Apply'
$button.Location = New-Object System.Drawing.Point(30,130)
$disabled = New-Object System.Windows.Forms.Button
$disabled.Name = 'fixture.disabled'; $disabled.Text = 'Disabled'; $disabled.AccessibleName = 'Disabled'; $disabled.Enabled = $false
$disabled.Location = New-Object System.Drawing.Point(180,130)
$check = New-Object System.Windows.Forms.CheckBox
$check.Name = 'fixture.option'; $check.Text = 'Enabled option'; $check.AccessibleName = 'Enabled option'; $check.Width = 220
$check.Location = New-Object System.Drawing.Point(30,190)
$script:clicks = 0
$button.Add_Click({ $script:clicks += 1 })
$form.Controls.AddRange(@($field,$secure,$button,$disabled,$check))
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 100
$timer.Add_Tick({
    $payload = @{pid=$PID; clicks=$script:clicks; text=$field.Text; checked=$check.Checked} | ConvertTo-Json -Compress
    # Reader tolerates an in-progress write; fixture contents contain no user data.
    [System.IO.File]::WriteAllText($OutputPath, $payload, (New-Object System.Text.UTF8Encoding($false)))
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)
