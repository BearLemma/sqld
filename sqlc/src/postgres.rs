use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use fn_error_context::context as fn_context;
use postgres_protocol::message::backend::DataRowBody;
use postgres_protocol::message::{backend, frontend};
use postgres_types::{BorrowToSql, Type};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as FmtWrite;
use std::io::prelude::*;
use std::net::TcpStream;
use std::ops::Range;
use std::rc::Rc;
use tracing::trace;
use tracing_subscriber::registry::Data;
use url::Url;

pub struct Database {
    pub conn: RefCell<Connection>,
}

impl Database {
    fn new(conn: Connection) -> Self {
        let conn = RefCell::new(conn);
        Self { conn }
    }
}

pub struct sqlite3 {
    pub inner: Rc<Database>,
}

impl sqlite3 {
    pub fn connect(addr: &str) -> Result<Self> {
        let conn = Connection::connect(addr)?;
        let inner = Rc::new(Database::new(conn));
        Ok(Self { inner })
    }
}

impl Drop for sqlite3 {
    fn drop(&mut self) {
        trace!("TRACE drop sqlite3");
    }
}

#[derive(Debug)]
pub enum StatementState {
    Prepared,
    Rows,
    Done,
}

pub struct Statement {
    pub db: sqlite3,
    pub sql: String,
    pub state: StatementState,
    pub metadata: Metadata,
    pub bound_params: Vec<BoundParam>,
    pub current_row: Option<(DataRowBody, Vec<Option<Range<usize>>>)>,
    pub rows: VecDeque<DataRowBody>,
}

impl Statement {
    pub fn new(parent: Rc<Database>, sql: String, metadata: Metadata) -> Self {
        Self {
            db: sqlite3 { inner: parent },
            sql,
            state: StatementState::Prepared,
            metadata,
            bound_params: vec![],
            current_row: None,
            rows: VecDeque::default(),
        }
    }

    pub fn parent_db(&self) -> &Rc<Database> {
        &self.db.inner
    }
}

#[derive(Debug)]
pub struct Metadata {
    pub col_names: Vec<String>,
    pub col_types: Vec<Type>,
}

impl Metadata {
    pub fn new() -> Metadata {
        let col_names = vec![];
        let col_types = vec![];
        Metadata {
            col_names,
            col_types,
        }
    }
}

pub enum ParamValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

pub struct BoundParam {
    pub index: usize,
    pub value: ParamValue,
}

#[derive(Debug)]
pub struct Connection {
    stream: TcpStream,
    rx_buf: BytesMut,
    username: String,
    password: Option<String>,
}

impl Connection {
    pub fn connect(addr: &str) -> Result<Self> {
        let url = Url::parse(addr)?;
        let host = url.host_str().unwrap();
        let port = url.port().unwrap();
        let password = url.password().map(|p| p.to_owned());
        let stream = TcpStream::connect((host, port))
            .with_context(|| format!("Unable to connect to {addr}"))?;
        let rx_buf = BytesMut::with_capacity(1024);
        let username = url.username().into();

        let mut conn = Self {
            stream,
            rx_buf,
            username,
            password,
        };
        conn.prepare_connection()?;
        Ok(conn)
    }

    fn prepare_connection(&mut self) -> Result<()> {
        trace!("postgres -> prepare_connection");
        self.send_startup()?;
        loop {
            let msg = self.receive_message()?;
            match msg {
                backend::Message::ReadyForQuery(_) => {
                    trace!("postgres -> prepare_connection -> ReadyForQuery");
                    return Ok(());
                }
                backend::Message::AuthenticationOk => {
                    trace!("postgres -> prepare_connection -> AuthenticationOk");
                }
                backend::Message::AuthenticationSasl(body) => {
                    trace!("postgres -> prepare_connection -> AuthenticationSasl");
                    self.run_sasl_auth(body)?;
                }
                backend::Message::AuthenticationCleartextPassword => todo!(),
                backend::Message::AuthenticationGss => todo!(),
                backend::Message::AuthenticationKerberosV5 => todo!(),
                backend::Message::AuthenticationMd5Password(_) => todo!(),
                backend::Message::ParameterStatus(_) | backend::Message::BackendKeyData(_) => {
                    trace!("postgres -> prepare_connection -> {}", get_msg_name(&msg));
                }
                _ => {
                    anyhow::bail!(
                        "Got unexpected message during while setting up connection: `{}`",
                        get_msg_name(&msg)
                    );
                }
            }
        }
    }

    pub fn prepare_stmt(&mut self, db: Rc<Database>, sql: String) -> Result<Statement> {
        trace!("postgres -> prepare_stmt");
        self.send_parse(&sql)?;
        self.send_flush()?;
        self.wait_until_parse_complete()?;
        self.send_describe()?;
        self.send_flush()?;
        let metadata = self.wait_until_row_description()?;
        Ok(Statement::new(db, sql, metadata))
    }

    pub fn execute(&mut self, params: Vec<BoundParam>) -> Result<VecDeque<DataRowBody>> {
        trace!("postgres -> execute");
        self.send_bind_params(params)?;
        self.send_execute()?;
        self.send_sync()?;
        self.receive_rows()
    }

    fn send_startup(&mut self) -> Result<()> {
        trace!("postgres -> send_startup");
        let mut msg = BytesMut::new();
        let mut params = HashMap::new();
        params.insert("user", self.username.as_str());
        frontend::startup_message(params.into_iter(), &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_parse(&mut self, sql: &str) -> Result<()> {
        trace!("postgres -> send_parse");
        let mut msg = BytesMut::new();
        let param_types = vec![];
        // FIXME: allocate a unique name for every statement and use it.
        frontend::parse("", sql, param_types, &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_bind_params(&mut self, mut params: Vec<BoundParam>) -> Result<()> {
        trace!("postgres -> send_bind_params");

        params.sort_by_key(|p| p.index);
        let portal = "";
        let statement_name = "";
        // Indicates whether or not the parameter is text (0) or binary (1)
        let param_formats: Vec<_> = params
            .iter()
            .map(|p| match p.value {
                ParamValue::Text(_) => 0,
                _ => 1,
            })
            .collect();

        let params = params.into_iter();
        let mut msg = BytesMut::new();
        frontend::bind(
            &portal,
            &statement_name,
            param_formats,
            params,
            |param, buf| {
                match param.value {
                    ParamValue::Null => return Ok(postgres_protocol::IsNull::Yes),
                    ParamValue::Text(txt) => buf
                        .write_str(&txt)
                        .context("failed to bind String parameter")?,
                    _ => todo!(),
                };
                Ok(postgres_protocol::IsNull::No)
            },
            Some(1),
            &mut msg,
        )
        .map_err(|_| anyhow!("bind failed"))?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_describe(&mut self) -> Result<()> {
        trace!("postgres -> send_describe");
        let mut msg = BytesMut::new();
        frontend::describe('S' as u8, "", &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_execute(&mut self) -> Result<()> {
        trace!("postgres -> send_execute");
        let mut msg = BytesMut::new();
        frontend::execute("", 0, &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_flush(&mut self) -> Result<()> {
        trace!("postgres -> send_flush");
        let mut msg = BytesMut::new();
        frontend::flush(&mut msg);
        self.stream.write_all(&msg)?;
        Ok(())
    }

    fn send_sync(&mut self) -> Result<()> {
        trace!("postgres -> send_sync");
        let mut msg = BytesMut::new();
        frontend::sync(&mut msg);
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn wait_until_parse_complete(&mut self) -> Result<()> {
        trace!("postgres -> wait_until_parse_complete");
        loop {
            let msg = self.receive_message()?;
            match msg {
                backend::Message::ParseComplete => {
                    trace!("TRACE postgres -> ParseComplete");
                    break;
                }
                _ => todo!(),
            }
        }
        Ok(())
    }

    pub fn wait_until_row_description(&mut self) -> Result<Metadata> {
        loop {
            let msg = self.receive_message()?;
            match msg {
                backend::Message::RowDescription(row_description) => {
                    let mut metadata = Metadata::new();
                    let mut fields = row_description.fields();
                    while let Some(field) = fields.next().unwrap() {
                        metadata.col_names.push(field.name().into());
                        let ty = Type::from_oid(field.type_oid()).unwrap();
                        metadata.col_types.push(ty);
                    }
                    return Ok(metadata);
                }
                backend::Message::ParameterDescription(_) => {
                    trace!("TRACE postgres -> wait_until_row_description -> ParameterDescription");
                }
                backend::Message::NoData => {
                    return Ok(Metadata::new());
                }
                _ => todo!(),
            }
        }
    }

    pub fn receive_rows(&mut self) -> Result<VecDeque<DataRowBody>> {
        let mut rows = VecDeque::default();
        loop {
            let msg = self.receive_message()?;
            match msg {
                backend::Message::DataRow(row) => {
                    trace!("postgres -> receive_rows -> DataRow");
                    rows.push_back(row);
                }
                backend::Message::ReadyForQuery(_) => {
                    trace!("postgres -> receive_rows -> ReadyForQuery");
                    return Ok(rows);
                }
                _ => unreachable!(),
            }
        }
    }

    pub fn wait_until_ready_for_query(&mut self) -> Result<()> {
        loop {
            let msg = self.receive_message()?;
            match msg {
                backend::Message::ReadyForQuery(_) => {
                    trace!("postgres -> ReadyForQuery");
                    return Ok(());
                }
                _ => unimplemented!(),
            }
        }
    }

    #[fn_context("failed to receive the next message from postgres server")]
    fn receive_message(&mut self) -> Result<backend::Message> {
        loop {
            let msg = backend::Message::parse(&mut self.rx_buf)?;
            match msg {
                Some(msg) => {
                    if let backend::Message::ErrorResponse(body) = msg {
                        trace!("postgres -> receive_message -> got error response");
                        anyhow::bail!(self.parse_err(body)?);
                    }
                    return Ok(msg);
                }
                None => {
                    // FIXME: Optimize with spare_capacity_mut() to make zero-copy.
                    let mut buf = [0u8; 1024];
                    let nr = self.stream.read(&mut buf)?;
                    self.rx_buf.extend_from_slice(&buf[0..nr]);
                }
            }
        }
    }

    #[fn_context("failed to authenticate to SQL server using SASL authentication protocol")]
    fn run_sasl_auth(&mut self, body: backend::AuthenticationSaslBody) -> Result<()> {
        let mechanisms: Vec<_> = body.mechanisms().collect()?;
        anyhow::ensure!(
            mechanisms.contains(&"SCRAM-SHA-256"),
            "our client supports only 'SCRAM-SHA-256' SASL auth protocol, but the server supports only {mechanisms:?}"
        );

        let username = self.username.clone();
        let password = self
            .password
            .clone()
            .context("password must be provided when server enforces SASL auth")?;
        let scram = scram::ScramClient::new(&username, &password, None);
        let (scram, cli_message) = scram.client_first();

        let mut buff = BytesMut::new();
        frontend::sasl_initial_response("SCRAM-SHA-256", cli_message.as_bytes(), &mut buff)?;
        self.stream.write_all(&buff)?;

        trace!("TRACE postgres -> AuthenticationSasl -> client first message sent");

        let body = match self.receive_message()? {
            backend::Message::AuthenticationSaslContinue(body) => body,
            _ => anyhow::bail!(
                "received unexpected message from server. Expected 'AuthenticationSaslContinue'.",
            ),
        };

        let scram = scram.handle_server_first(std::str::from_utf8(body.data())?)?;
        let (scram, client_final) = scram.client_final();

        buff.clear();
        frontend::sasl_response(client_final.as_bytes(), &mut buff)?;
        self.stream.write_all(&buff)?;

        // Receive the last message from server.
        let body = match self.receive_message()? {
            backend::Message::AuthenticationSaslFinal(body) => body,
            _ => anyhow::bail!(
                "received unexpected message from server. Expected 'AuthenticationSaslFinal'.",
            ),
        };

        // Checks the final response from the server
        scram.handle_server_final(std::str::from_utf8(body.data())?)?;

        match self.receive_message()? {
            backend::Message::AuthenticationOk => {
                trace!("TRACE postgres -> AuthenticationSasl -> authentication successful");
                Ok(())
            }
            _ => anyhow::bail!(
                "received unexpected message from server. Expected 'AuthenticationOk'.",
            ),
        }
    }

    fn parse_err(&self, body: backend::ErrorResponseBody) -> Result<String> {
        let err_fields: Vec<_> = body.fields().map(|f| Ok(f.value().to_string())).collect()?;
        let err_msg =
            format!("server responded with error response. Provided error fields: {err_fields:?}");
        trace!("TRACE postgres -> Error ocurred: {err_msg}");
        Ok(err_msg)
    }
}

fn get_msg_name(msg: &backend::Message) -> &str {
    match msg {
        backend::Message::AuthenticationCleartextPassword => "AuthenticationCleartextPassword",
        backend::Message::AuthenticationGss => "AuthenticationGss",
        backend::Message::AuthenticationKerberosV5 => "AuthenticationKerberosV5",
        backend::Message::AuthenticationMd5Password(_) => "AuthenticationMd5Password",
        backend::Message::AuthenticationOk => "AuthenticationOk",
        backend::Message::AuthenticationScmCredential => "AuthenticationScmCredential",
        backend::Message::AuthenticationSspi => "AuthenticationSspi",
        backend::Message::AuthenticationGssContinue(_) => "AuthenticationGssContinue",
        backend::Message::AuthenticationSasl(_) => "AuthenticationSasl",
        backend::Message::AuthenticationSaslContinue(_) => "AuthenticationSaslContinue",
        backend::Message::AuthenticationSaslFinal(_) => "AuthenticationSaslFinal",
        backend::Message::BackendKeyData(_) => "BackendKeyData",
        backend::Message::BindComplete => "BindComplete",
        backend::Message::CloseComplete => "CloseComplete",
        backend::Message::CommandComplete(_) => "CommandComplete",
        backend::Message::CopyData(_) => "CopyData",
        backend::Message::CopyDone => "CopyDone",
        backend::Message::CopyInResponse(_) => "CopyInResponse",
        backend::Message::CopyOutResponse(_) => "CopyOutResponse",
        backend::Message::DataRow(_) => "DataRow",
        backend::Message::EmptyQueryResponse => "EmptyQueryResponse",
        backend::Message::ErrorResponse(_) => "ErrorResponse",
        backend::Message::NoData => "NoData",
        backend::Message::NoticeResponse(_) => "NoticeResponse",
        backend::Message::NotificationResponse(_) => "NotificationResponse",
        backend::Message::ParameterDescription(_) => "ParameterDescription",
        backend::Message::ParameterStatus(_) => "ParameterStatus",
        backend::Message::ParseComplete => "ParseComplete",
        backend::Message::PortalSuspended => "PortalSuspended",
        backend::Message::ReadyForQuery(_) => "ReadyForQuery",
        backend::Message::RowDescription(_) => "RowDescription",
        _ => "-- Unknown --",
    }
}
