require "sqlite3"

db_uri = ENV["DB_URI"]

if db_uri == ""
  throw "Please configure database via DB_URI environment variable."
end

describe "SQLite3 client" do
  it "connects" do
    db = SQLite3::Database.open db_uri
  end

  it "performs schema changes" do
    db = SQLite3::Database.open db_uri
    db.execute("CREATE TABLE IF NOT EXISTS users (username TEXT, pass TEXT)")
  end

  it "queries tables" do
    db = SQLite3::Database.open db_uri
    db.execute("CREATE TABLE IF NOT EXISTS users (username TEXT, pass TEXT)")
    db.execute("CREATE TABLE IF NOT EXISTS users (username TEXT, pass TEXT)")
    db.execute("SELECT * FROM users") do |results|
      puts results
    end
  end

  it "inserts and reads some data using prepared stmt" do
    db = SQLite3::Database.open db_uri
    db.execute("CREATE TABLE IF NOT EXISTS users (username TEXT, pass TEXT)")
    db.execute("DELETE FROM users")

    insert_stmt = db.execute("INSERT INTO users (username, pass) VALUES ('Jan', 'yes')")
    insert_stmt = db.prepare("INSERT INTO users (username, pass) VALUES ($1, $2)")
    insert_stmt.execute("Jan", "yes")
    insert_stmt.execute("Rudolf", "no")

    puts "==================="
    puts "==================="
    puts "==================="
    select_stmt = db.prepare("SELECT username FROM users WHERE pass = $1")
    raise "Unexpected number of stmt columns" unless select_stmt.columns == ["username"]

    row = select_stmt.execute("yes").next
    puts "LOLOLOLO", row

    results = select_stmt.execute("no")
    puts results
  end
end
