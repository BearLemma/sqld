import { Driver } from "./driver/Driver";
import { HttpDriver } from "./driver/HttpDriver";
import { SqliteDriver } from "./driver/SqliteDriver";

export type Config = {
    url: string,
};

/**
 * A SQL query result set row.
 */
export type Row = Record<string, SqlValue>;
export type SqlValue = string | number | boolean | null;
export type Params = SqlValue[] | Record<string, SqlValue>;
export type BoundStatement = { sql: string, params: Params };

/**
 * A SQL query result set.
 */
export type ResultSet = {
    /**
     * Was the query successful?
     * If true, rows and columns are provided.
     * If false, error is provided
     */
    success: boolean;
    /**
     * Query result columns.
     */
    columns?: string[];
    /**
     * Query results.
     */
    rows?: Row[];
    /**
     * Error information, if not successful.
     */
    error?: {
        message: string;
    };
    /**
     * Extra information about the query results.
     */
    meta: {
        duration: number;
    };
};

/**
 * A libSQL database connection.
 */
export class Connection {
    private driver: Driver;

    constructor(driver: Driver) {
        this.driver = driver;
    }

    /**
     * Execute a SQL statement in a transaction.
     */
    async execute(sql: string, params?: Params): Promise<ResultSet> {
        return this.driver.execute(sql, params);
    }

    /**
     * Execute a batch of SQL statements in a transaction.
     */
    async transaction(stmts: string[] | BoundStatement[]): Promise<ResultSet[]> {
        return await this.driver.transaction(stmts);
    }
}

export function connect(config: Config): Connection {
    const rawUrl = config.url;
    const url = new URL(rawUrl);
    if (url.protocol == "http:" || url.protocol == "https:") {
        return new Connection(new HttpDriver(url));
    } else {
        return new Connection(new SqliteDriver(rawUrl));
    }
}
