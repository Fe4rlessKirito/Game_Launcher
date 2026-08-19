using Launcher.Core;
using Microsoft.Data.Sqlite;

namespace Launcher.Storage;

public sealed class LocalStateStore(string databasePath)
{
    static LocalStateStore() => SQLitePCL.Batteries_V2.Init();

    private readonly string _connectionString = new SqliteConnectionStringBuilder { DataSource = databasePath, Mode = SqliteOpenMode.ReadWriteCreate, Cache = SqliteCacheMode.Shared }.ToString();

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(databasePath))!);
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = """
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
            INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, $applied_at);
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS library_exclusions (
                game_id TEXT PRIMARY KEY,
                excluded_at TEXT NOT NULL
            );
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
                updated_at TEXT NOT NULL,
                last_error TEXT
            );
            """;
        command.Parameters.AddWithValue("$applied_at", DateTimeOffset.UtcNow.ToString("O"));
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
        await EnsureColumnAsync(connection, "download_jobs", "last_error", "TEXT", cancellationToken).ConfigureAwait(false);
    }

    public async Task<IReadOnlySet<string>> GetExcludedGameIdsAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT game_id FROM library_exclusions";
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        var result = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false))
        {
            var gameId = reader.GetString(0);
            if (!string.IsNullOrWhiteSpace(gameId)) result.Add(gameId);
        }

        return result;
    }

    public async Task SetGameExcludedAsync(string gameId, bool excluded, CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(gameId)) throw new ArgumentException("A game id is required.", nameof(gameId));

        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        if (excluded)
        {
            command.CommandText = "INSERT INTO library_exclusions(game_id, excluded_at) VALUES($game_id, $excluded_at) ON CONFLICT(game_id) DO UPDATE SET excluded_at=excluded.excluded_at";
            command.Parameters.AddWithValue("$game_id", gameId);
            command.Parameters.AddWithValue("$excluded_at", DateTimeOffset.UtcNow.ToString("O"));
        }
        else
        {
            command.CommandText = "DELETE FROM library_exclusions WHERE game_id = $game_id";
            command.Parameters.AddWithValue("$game_id", gameId);
        }

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

    public async Task SaveDownloadJobAsync(PersistedDownloadJob job, CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "INSERT INTO download_jobs(job_id, build_id, state, downloaded_bytes, total_bytes, updated_at, last_error) VALUES($job_id, $build_id, $state, $downloaded_bytes, $total_bytes, $updated_at, $last_error) ON CONFLICT(job_id) DO UPDATE SET build_id=excluded.build_id, state=excluded.state, downloaded_bytes=excluded.downloaded_bytes, total_bytes=excluded.total_bytes, updated_at=excluded.updated_at, last_error=excluded.last_error";
        command.Parameters.AddWithValue("$job_id", job.JobId);
        command.Parameters.AddWithValue("$build_id", job.BuildId);
        command.Parameters.AddWithValue("$state", job.State.ToString());
        command.Parameters.AddWithValue("$downloaded_bytes", job.DownloadedBytes);
        command.Parameters.AddWithValue("$total_bytes", job.TotalBytes);
        command.Parameters.AddWithValue("$updated_at", job.UpdatedAt.ToUniversalTime().ToString("O"));
        command.Parameters.AddWithValue("$last_error", (object?)job.LastError ?? DBNull.Value);
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<PersistedDownloadJob?> GetDownloadJobAsync(string jobId, CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT job_id, build_id, state, downloaded_bytes, total_bytes, updated_at, last_error FROM download_jobs WHERE job_id = $job_id";
        command.Parameters.AddWithValue("$job_id", jobId);
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        if (!await reader.ReadAsync(cancellationToken).ConfigureAwait(false)) return null;
        return new PersistedDownloadJob(reader.GetString(0), reader.GetString(1), Enum.Parse<DownloadJobState>(reader.GetString(2)), reader.GetInt64(3), reader.GetInt64(4), DateTimeOffset.Parse(reader.GetString(5), System.Globalization.CultureInfo.InvariantCulture), reader.IsDBNull(6) ? null : reader.GetString(6));
    }

    public async Task<IReadOnlyList<PersistedDownloadJob>> GetDownloadJobsAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "SELECT job_id, build_id, state, downloaded_bytes, total_bytes, updated_at, last_error FROM download_jobs ORDER BY updated_at DESC";
        await using var reader = await command.ExecuteReaderAsync(cancellationToken).ConfigureAwait(false);
        var jobs = new List<PersistedDownloadJob>();
        while (await reader.ReadAsync(cancellationToken).ConfigureAwait(false)) jobs.Add(new PersistedDownloadJob(reader.GetString(0), reader.GetString(1), Enum.Parse<DownloadJobState>(reader.GetString(2)), reader.GetInt64(3), reader.GetInt64(4), DateTimeOffset.Parse(reader.GetString(5), System.Globalization.CultureInfo.InvariantCulture), reader.IsDBNull(6) ? null : reader.GetString(6)));
        return jobs;
    }

    public async Task<int> FailInterruptedDownloadJobsAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "UPDATE download_jobs SET state = $state, last_error = $last_error, updated_at = $updated_at WHERE state NOT IN ($ready, $failed, $cancelled)";
        command.Parameters.AddWithValue("$state", DownloadJobState.Failed.ToString());
        command.Parameters.AddWithValue("$last_error", "Download was interrupted; choose Install to retry.");
        command.Parameters.AddWithValue("$updated_at", DateTimeOffset.UtcNow.ToString("O"));
        command.Parameters.AddWithValue("$ready", DownloadJobState.Ready.ToString());
        command.Parameters.AddWithValue("$failed", DownloadJobState.Failed.ToString());
        command.Parameters.AddWithValue("$cancelled", DownloadJobState.Cancelled.ToString());
        return await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task DeleteDownloadJobAsync(string jobId, CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "DELETE FROM download_jobs WHERE job_id = $job_id";
        command.Parameters.AddWithValue("$job_id", jobId);
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task DeleteCompletedDownloadJobsAsync(CancellationToken cancellationToken = default)
    {
        await using var connection = new SqliteConnection(_connectionString);
        await connection.OpenAsync(cancellationToken).ConfigureAwait(false);
        await using var command = connection.CreateCommand();
        command.CommandText = "DELETE FROM download_jobs WHERE state = $state";
        command.Parameters.AddWithValue("$state", DownloadJobState.Ready.ToString());
        await command.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }

    private static async Task EnsureColumnAsync(SqliteConnection connection, string table, string column, string type, CancellationToken cancellationToken)
    {
        await using var check = connection.CreateCommand();
        check.CommandText = $"SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = $column";
        check.Parameters.AddWithValue("$column", column);
        var exists = Convert.ToInt64(await check.ExecuteScalarAsync(cancellationToken).ConfigureAwait(false), System.Globalization.CultureInfo.InvariantCulture) != 0;
        if (exists) return;
        await using var alter = connection.CreateCommand();
        alter.CommandText = $"ALTER TABLE {table} ADD COLUMN {column} {type}";
        await alter.ExecuteNonQueryAsync(cancellationToken).ConfigureAwait(false);
    }
}
