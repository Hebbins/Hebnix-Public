<?php

declare(strict_types=1);

$config = require dirname(__DIR__) . '/config.php';
$db = new PDO($config['database_dsn'], $config['database_user'], $config['database_password']);
$db->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);
$db->exec(file_get_contents(dirname(__DIR__) . '/schema.sql'));

function reply(int $status, array $body): never {
    http_response_code($status);
    header('content-type: application/json');
    echo json_encode($body, JSON_THROW_ON_ERROR);
    exit;
}

function input(): array {
    $body = json_decode(file_get_contents('php://input'), true);
    if (!is_array($body)) reply(400, ['error' => 'invalid json body']);
    return $body;
}

function token(): string {
    return rtrim(strtr(base64_encode(random_bytes(24)), '+/', '-_'), '=');
}

function present(array $row): array {
    return ['pin' => $row['pin'], 'host_name' => $row['host_name'], 'endpoint' => ['host' => $row['host'], 'port' => (int) $row['port']], 'map' => ['id' => $row['map_id'], 'name' => $row['map_name'], 'sha256' => $row['map_sha256'], 'download_url' => $row['map_download_url']], 'join_token' => $row['join_token'], 'expires_at' => $row['expires_at']];
}

function find_room(PDO $db, string $pin): ?array {
    $statement = $db->prepare('SELECT * FROM rooms WHERE pin = ? AND expires_at > CURRENT_TIMESTAMP');
    $statement->execute([$pin]);
    $row = $statement->fetch(PDO::FETCH_ASSOC);
    return $row === false ? null : $row;
}

$db->exec('DELETE FROM rooms WHERE expires_at <= CURRENT_TIMESTAMP');
$method = $_SERVER['REQUEST_METHOD'];
$path = parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH);

if ($method === 'POST' && $path === '/v1/lan/rooms') {
    $body = input(); $endpoint = $body['endpoint'] ?? []; $map = $body['map'] ?? [];
    $hostName = trim((string) ($body['host_name'] ?? '')); $host = trim((string) ($endpoint['host'] ?? ''));
    $port = filter_var($endpoint['port'] ?? null, FILTER_VALIDATE_INT, ['options' => ['min_range' => 1, 'max_range' => 65535]]);
    $mapId = trim((string) ($map['id'] ?? '')); $mapName = trim((string) ($map['name'] ?? ''));
    $hash = strtolower(trim((string) ($map['sha256'] ?? ''))); $url = trim((string) ($map['download_url'] ?? ''));
    $version = filter_var($body['protocol_version'] ?? null, FILTER_VALIDATE_INT, ['options' => ['min_range' => 1]]);
    if ($hostName === '' || filter_var($host, FILTER_VALIDATE_IP) === false || $port === false || $mapId === '' || $mapName === '' || preg_match('/^[a-f0-9]{64}$/', $hash) !== 1 || filter_var($url, FILTER_VALIDATE_URL) === false || $version === false) reply(422, ['error' => 'invalid room details']);
    $expiresAt = gmdate('Y-m-d H:i:s', time() + (int) $config['room_ttl_seconds']);
    for ($attempt = 0; $attempt < 10; $attempt++) {
        $pin = (string) random_int(100000, 999999); $secret = token(); $joinToken = token();
        try {
            $statement = $db->prepare('INSERT INTO rooms (pin, host_secret, join_token, host_name, host, port, map_id, map_name, map_sha256, map_download_url, protocol_version, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)');
            $statement->execute([$pin, $secret, $joinToken, $hostName, $host, $port, $mapId, $mapName, $hash, $url, $version, $expiresAt]);
            $row = find_room($db, $pin);
            reply(201, ['room' => present($row), 'credentials' => ['pin' => $pin, 'host_secret' => $secret]]);
        } catch (PDOException $error) {
            if ($error->getCode() !== '23000') throw $error;
        }
    }
    reply(503, ['error' => 'could not allocate pin']);
}

if ($method === 'GET' && preg_match('#^/v1/lan/rooms/(\\d{6})$#', $path, $matches) === 1) {
    $row = find_room($db, $matches[1]);
    if ($row === null) reply(404, ['error' => 'room not found']);
    reply(200, present($row));
}

if ($method === 'POST' && preg_match('#^/v1/lan/rooms/(\\d{6})/heartbeat$#', $path, $matches) === 1) {
    $body = input(); $row = find_room($db, $matches[1]);
    if ($row === null || !hash_equals($row['host_secret'], (string) ($body['host_secret'] ?? ''))) reply(404, ['error' => 'room not found']);
    $expiresAt = gmdate('Y-m-d H:i:s', time() + (int) $config['room_ttl_seconds']);
    $statement = $db->prepare('UPDATE rooms SET expires_at = ? WHERE pin = ?'); $statement->execute([$expiresAt, $matches[1]]); $row['expires_at'] = $expiresAt;
    reply(200, present($row));
}

if ($method === 'DELETE' && preg_match('#^/v1/lan/rooms/(\\d{6})$#', $path, $matches) === 1) {
    $body = input(); $row = find_room($db, $matches[1]);
    if ($row === null || !hash_equals($row['host_secret'], (string) ($body['host_secret'] ?? ''))) reply(404, ['error' => 'room not found']);
    $statement = $db->prepare('DELETE FROM rooms WHERE pin = ?'); $statement->execute([$matches[1]]);
    reply(200, ['closed' => true]);
}

reply(404, ['error' => 'route not found']);
