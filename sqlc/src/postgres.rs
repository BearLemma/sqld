use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use fn_error_context::context as fn_context;
use postgres_protocol::message::backend::DataRowBody;
use postgres_protocol::message::{backend, frontend};
use postgres_types::{BorrowToSql, Type};
use std::collections::{HashMap, VecDeque};
use std::io::prelude::*;
use std::net::TcpStream;
use tracing::trace;
use url::Url;

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
        Ok(Self {
            stream,
            rx_buf,
            username,
            password,
        })
    }

    pub fn send_startup(&mut self) -> Result<()> {
        let mut msg = BytesMut::new();
        let mut params = HashMap::new();
        params.insert("user", self.username.as_str());
        frontend::startup_message(params.into_iter(), &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_parse(&mut self, sql: &str) -> Result<()> {
        let mut msg = BytesMut::new();
        let param_types = vec![];
        // FIXME: allocate a unique name for every statement and use it.
        frontend::parse("", sql, param_types, &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_bind<P, I>(&mut self, params: I) -> Result<()>
    where
        P: BorrowToSql,
        I: IntoIterator<Item = P>,
    {
        let mut msg = BytesMut::new();
        let portal = "";
        let statement = "";
        let param_formats = vec![];
        let param_types: Vec<Type> = vec![];
        let params = params.into_iter();
        frontend::bind(
            &portal,
            &statement,
            param_formats,
            params.zip(param_types).enumerate(),
            |(_idx, (_param, _ty)), _buf| Ok(postgres_protocol::IsNull::No),
            Some(1),
            &mut msg,
        )
        .map_err(|_| anyhow!("bind failed"))?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_describe(&mut self) -> Result<()> {
        let mut msg = BytesMut::new();
        frontend::describe('S' as u8, "", &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_execute(&mut self) -> Result<()> {
        let mut msg = BytesMut::new();
        frontend::execute("", 0, &mut msg)?;
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_flush(&mut self) -> Result<()> {
        let mut msg = BytesMut::new();
        frontend::flush(&mut msg);
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn send_sync(&mut self) -> Result<()> {
        let mut msg = BytesMut::new();
        frontend::sync(&mut msg);
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn wait_until_parse_complete(&mut self) -> Result<()> {
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

    pub fn wait_until_ready(&mut self) -> Result<VecDeque<DataRowBody>> {
        let mut metadata = Metadata::new();
        let mut rows = VecDeque::default();
        loop {
            let msg = self.receive_message()?;
            if !self.process_msg(msg, &mut metadata, &mut rows)? {
                return Ok(rows);
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

    fn process_msg(
        &mut self,
        msg: backend::Message,
        metadata: &mut Metadata,
        rows: &mut VecDeque<DataRowBody>,
    ) -> Result<bool> {
        match msg {
            backend::Message::AuthenticationCleartextPassword => todo!(),
            backend::Message::AuthenticationGss => todo!(),
            backend::Message::AuthenticationKerberosV5 => todo!(),
            backend::Message::AuthenticationMd5Password(_) => todo!(),
            backend::Message::AuthenticationOk => {
                trace!("TRACE postgres -> AuthenticationOk");
            }
            backend::Message::AuthenticationScmCredential => todo!(),
            backend::Message::AuthenticationSspi => todo!(),
            backend::Message::AuthenticationGssContinue(_) => todo!(),
            backend::Message::AuthenticationSasl(body) => {
                trace!("TRACE postgres -> AuthenticationSasl");
                self.run_sasl_auth(body)?;
            }
            backend::Message::AuthenticationSaslContinue(_) => todo!(),
            backend::Message::AuthenticationSaslFinal(_) => todo!(),
            backend::Message::BackendKeyData(_) => {
                trace!("TRACE postgres -> BackendKeyData");
            }
            backend::Message::BindComplete => {
                trace!("TRACE postgres -> BindComplete");
            }
            backend::Message::CloseComplete => todo!(),
            backend::Message::CommandComplete(_) => {
                trace!("TRACE postgres -> CommandComplete");
            }
            backend::Message::CopyData(_) => todo!(),
            backend::Message::CopyDone => todo!(),
            backend::Message::CopyInResponse(_) => todo!(),
            backend::Message::CopyOutResponse(_) => todo!(),
            backend::Message::DataRow(row) => {
                trace!("TRACE postgres -> DataRow");
                rows.push_back(row);
            }
            backend::Message::EmptyQueryResponse => todo!(),
            backend::Message::ErrorResponse(body) => {
                trace!("TRACE postgres -> ErrorResponse");
                anyhow::bail!(self.parse_err(body)?)
            }
            backend::Message::NoData => {
                trace!("TRACE postgres -> NoData");
            }
            backend::Message::NoticeResponse(_) => {
                trace!("TRACE postgres -> NoticeResponse");
            }
            backend::Message::NotificationResponse(_) => {
                trace!("TRACE postgres -> NotificationResponse");
            }
            backend::Message::ParameterDescription(_) => {
                trace!("TRACE postgres -> ParameterDescription");
            }
            backend::Message::ParameterStatus(_) => {
                trace!("TRACE postgres -> ParameterStatus");
            }
            backend::Message::ParseComplete => {
                trace!("TRACE postgres -> ParseComplete");
            }
            backend::Message::PortalSuspended => todo!(),
            backend::Message::ReadyForQuery(_) => {
                trace!("TRACE postgres -> ReadyForQuery");
                return Ok(false);
            }
            backend::Message::RowDescription(row_description) => {
                trace!("TRACE postgres -> RowDescription");
                let mut fields = row_description.fields();
                while let Some(field) = fields.next()? {
                    metadata.col_names.push(field.name().into());
                    let ty = Type::from_oid(field.type_oid()).unwrap();
                    metadata.col_types.push(ty);
                }
            }
            _ => todo!(),
        }
        Ok(true)
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

        trace!("TRACE postgres -> AuthenticationSasl -> authentication successful");
        Ok(())
    }

    fn parse_err(&self, body: backend::ErrorResponseBody) -> Result<String> {
        let err_fields: Vec<_> = body.fields().map(|f| Ok(f.value().to_string())).collect()?;
        let err_msg =
            format!("server responded with error response. Provided error fields: {err_fields:?}");
        trace!("TRACE postgres -> Error ocurred: {err_msg}");
        Ok(err_msg)
    }
}
