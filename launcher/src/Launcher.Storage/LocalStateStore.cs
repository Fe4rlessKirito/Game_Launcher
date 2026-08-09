using Launcher.Core;
using Microsoft.Data.Sqlite;

namespace Launcher.Storage;

public sealed class LocalStateStore(string databasePath)
{
    static LocalStateStore() => SQLitePCL.Batteries_V2.Init();

    private readonly string _connectionString = new SqliteConnectionStringBuilder { DataSource = databasePath, Mode = SqliteOpenMode.ReadWriteCreate, Cache = SqliteCacheMode.Shared }.ToString();

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS installed_games (
                game_id TEXT PRIMARY KEY,
                build_id TEXT NOT NULL,
                display_version TEXT NOT NULL,
                install_root TEXT NOT NULL,
                manifest_json TEXT NOT NULL,
                installed_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS download_jobs (
                job_id TEXT PRIMARY KEY,
                build_id TEXT NOT NULL,
                state TEXT NOT NULL,
                downloaded_bytes INTEGER NOT NULL DEFAULT 0,
                total_bytes INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );
            """;
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<InstalledGame>> GetInstalledGamesAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT game_id, build_id, display_version, install_root, manifest_json, installed_at FROM installed_games ORDER BY installed_at DESC";
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        var result = new List<InstalledGame>();
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false)) result.Add(new InstalledGame(reader.GetString(0), reader.GetString(1), reader.GetString(2), reader.GetString(3), reader.GetString(4), DateTimeOffset.Parse(reader.GetString(5), System.Globalization.CultureInfo.InvariantCulture)));
        return result;
    }

    public async Task SaveInstalledGameAsync(InstalledGame game, CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var transaction = (Microsoft.Data.Sqlite.SqliteTransaction)await connection.BeginTransactionAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = "INSERT INTO installed_games(game_id, build_id, display_version, install_root, manifest_json, installed_at) VALUES($game_id, $build_id, $display_version, $install_root, $manifest_json, $installed_at) ON CONFLICT(game_id) DO UPDATE SET build_id=excluded.build_id, display_version=excluded.display_version, install_root=excluded.install_root, manifest_json=excluded.manifest_json, installed_at=excluded.installed_at";
        command.Parameters.AddWithValue("$game_id", game.GameId);
        command.Parameters.AddWithValue("$build_id", game.BuildId);
        command.Parameters.AddWithValue("$display_version", game.DisplayVersion);
        command.Parameters.AddWithValue("$install_root", game.InstallRoot);
        command.Parameters.AddWithValue("$manifest_json", game.ManifestJson);
        command.Parameters.AddWithValue("$installed_at", game.InstalledAt.ToUniversalTime().ToString("O"));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        await transaction.CommitAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task RemoveInstalledGameAsync(string gameId, CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "DELETE FROM installed_games WHERE game_id = $game_id";
        command.Parameters.AddWithValue("$game_id", gameId);
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }
}
