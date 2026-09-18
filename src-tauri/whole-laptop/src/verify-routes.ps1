# Read-only: never edit another adapter or remove a competing route.
$adapter = @(Get-NetAdapter -IncludeHidden -ErrorAction Stop | Where-Object { $_.Name -eq 'AetherWholeLaptop' })
if ($adapter.Count -ne 1 -or $adapter[0].Status -ne 'Up') {
    throw 'The AetherWholeLaptop adapter is missing or is not Up.'
}
$index = $adapter[0].ifIndex
$routes = @(Get-NetRoute -InterfaceIndex $index -PolicyStore ActiveStore -ErrorAction Stop)
foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1', '::/1', '8000::/1')) {
    if (!($routes | Where-Object { $_.DestinationPrefix -eq $prefix })) {
        throw "The AetherWholeLaptop adapter is missing its $prefix capture route. Windows traffic capture was not confirmed."
    }
}
# Find-NetRoute returns a source-address object and a route object. Check the
# selected route without constraining InterfaceIndex, which would hide a conflict.
foreach ($destination in @('1.1.1.1', '208.67.222.222', '2606:4700:4700::1111')) {
    $best = @(Find-NetRoute -RemoteIPAddress $destination -ErrorAction Stop | Where-Object { $_.DestinationPrefix })
    if ($best.Count -ne 1) {
        throw "Windows did not return one selected route for $destination."
    }
    if ($best[0].InterfaceIndex -ne $index) {
        throw "Windows selected '$($best[0].InterfaceAlias)' ($($best[0].DestinationPrefix)) for $destination instead of AetherWholeLaptop. Close other VPN/TUN apps and retry. No other adapter was changed."
    }
}
Write-Output "Windows selected AetherWholeLaptop (index $index) for the IPv4 and IPv6 route checks."
