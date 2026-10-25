//! The RDP client, on IronRDP, in the viewer process.
//!
//! The server's bytes arrive from the window as [`ToViewer::Net`] and the
//! bytes for the server go back as [`FromViewer::Net`]: [`Pipe`] makes the
//! two look like one socket to IronRDP and to rustls. TLS and NLA
//! (CredSSP, NTLM) run here, in the sandbox, like the rest of the parsing.
//!
//! The server's TLS certificate is not checked against any authority: the
//! tunnel ends on the server itself, whose SSH host key FeroSSH already
//! checked, and xrdp and Windows use self-signed certificates anyway. TLS
//! still verifies that the server holds the certificate's key, and NLA
//! binds the login to that key.
//!
//! Keys go to the server as scan codes, read with the keyboard layout the
//! window reports, so the server types what the PC's keyboard shows.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, mpsc};

use ironrdp_blocking::Framed;
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::sspi;
use ironrdp_connector::{
    ClientConnector, ConnectionOutcome, ConnectionResult, Credentials, DesktopSize, Sequence as _,
    ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::capability_sets::{InputFlags, MajorPlatformType, RailSupportLevel};
use ironrdp_pdu::rdp::client_info::PerformanceFlags;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use rustls::pki_types;
use zeroize::Zeroizing;

use crate::pipe::{FromViewer, MAX_NET, ToViewer, read_frame};
use crate::viewer::{send, send_pixels};
use crate::{MAX_PIXELS, MAX_SIDE};

/// What the window's reader thread passes on.
enum Item {
    Msg(ToViewer),
    /// A frame that does not decode.
    Bad,
    /// The window closed the pipe.
    End,
}

/// The server connection as IronRDP and rustls see it: reads take the
/// window's [`ToViewer::Net`] bytes, writes become [`FromViewer::Net`].
/// Everything else from the window waits in `pending`; while
/// `interruptible`, a read that meets it returns `WouldBlock` so the
/// session can act on the input at once.
struct Pipe<'a, W: Write> {
    from: mpsc::Receiver<Item>,
    out: &'a mut W,
    inbox: VecDeque<u8>,
    pending: Vec<ToViewer>,
    interruptible: bool,
    closed: bool,
    bad: bool,
}

impl<W: Write> Read for Pipe<'_, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.inbox.is_empty() {
                let n = buf.len().min(self.inbox.len());
                for (b, v) in buf.iter_mut().zip(self.inbox.drain(..n)) {
                    *b = v;
                }
                return Ok(n);
            }
            if self.closed {
                return Ok(0);
            }
            match self.from.recv() {
                Ok(Item::Msg(ToViewer::Net(bytes))) => self.inbox.extend(bytes),
                Ok(Item::Msg(ToViewer::Closed) | Item::End) | Err(_) => self.closed = true,
                Ok(Item::Bad) => {
                    self.bad = true;
                    self.closed = true;
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame"));
                }
                Ok(Item::Msg(other)) => {
                    self.pending.push(other);
                    if self.interruptible {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                }
            }
        }
    }
}

impl<W: Write> Write for Pipe<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(MAX_NET);
        let chunk = buf.get(..n).unwrap_or_default();
        send(self.out, &FromViewer::Net(chunk.to_vec()))?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

type Tls<'p, 'a, W> = rustls::StreamOwned<rustls::ClientConnection, &'p mut Pipe<'a, W>>;

/// Why a session ended, for the window.
type Failure = String;

/// Runs an RDP session: asks the window for the password (and the user
/// name), connects, then shows the desktop until either side ends it.
pub(crate) fn session(
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
    (width, height): (u16, u16),
    layout: u32,
    user: String,
) -> io::Result<()> {
    let (tx, from) = mpsc::channel();
    std::thread::Builder::new()
        .name("viewer-input".into())
        .spawn(move || {
            loop {
                let item = match read_frame(&mut input) {
                    Ok(Some(f)) => ToViewer::decode(&f).map_or(Item::Bad, Item::Msg),
                    Ok(None) | Err(_) => Item::End,
                };
                let last = !matches!(item, Item::Msg(_));
                if tx.send(item).is_err() || last {
                    return;
                }
            }
        })?;

    send(&mut output, &FromViewer::NeedPassword)?;
    let (mut user, mut width, mut height) = (user, width, height);
    let password = loop {
        match from.recv() {
            Ok(Item::Msg(ToViewer::User(u))) => user = u,
            // The pane changed size during the login.
            Ok(Item::Msg(ToViewer::Resize {
                width: w,
                height: h,
            })) => (width, height) = (w, h),
            Ok(Item::Msg(ToViewer::Password(p))) => {
                let p = Zeroizing::new(p);
                break Zeroizing::new(String::from_utf8_lossy(&p).into_owned());
            }
            Ok(Item::Msg(ToViewer::Closed)) => {
                return send(
                    &mut output,
                    &FromViewer::Failed("The RDP server closed the connection.".into()),
                );
            }
            Ok(Item::Bad) => return nonsense(&mut output),
            Ok(Item::End) | Err(_) => return Ok(()),
            // Server bytes before the client spoke, and early input.
            Ok(Item::Msg(_)) => {}
        }
    };

    let mut pipe = Pipe {
        from,
        out: &mut output,
        inbox: VecDeque::new(),
        pending: Vec::new(),
        interruptible: false,
        closed: false,
        bad: false,
    };
    let (width, height) = desktop_size(width, height);
    let size = DesktopSize { width, height };
    let result = run(&mut pipe, config(user, password, size, layout));
    let bad = pipe.bad;
    drop(pipe);
    match result {
        _ if bad => nonsense(&mut output),
        Ok(()) => Ok(()),
        Err(why) => send(&mut output, &FromViewer::Failed(why)),
    }
}

/// A screen size RDP takes: 200 to [`MAX_SIDE`] pixels each way, the
/// width even.
fn desktop_size(width: u16, height: u16) -> (u16, u16) {
    (width.clamp(200, MAX_SIDE) & !1, height.clamp(200, MAX_SIDE))
}

fn nonsense(output: &mut impl Write) -> io::Result<()> {
    send(
        output,
        &FromViewer::Failed("The window sent the viewer nonsense.".into()),
    )?;
    Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame"))
}

fn config(
    username: String,
    password: Zeroizing<String>,
    desktop_size: DesktopSize,
    keyboard_layout: u32,
) -> ironrdp_connector::Config {
    ironrdp_connector::Config {
        desktop_size,
        monitor_layout: None,
        desktop_scale_factor: 0,
        // NLA where the server has it (Windows, GNOME Remote Desktop);
        // plain TLS for xrdp, which has no NLA. Then the login rides in
        // the client info (autologon) instead of the server's login
        // screen.
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword {
            username,
            password: password.to_string(),
        },
        domain: None,
        // xrdp resizes only clients that say they are newer than build
        // 419; any build will do.
        client_build: 2600,
        client_name: "FeroSSH".into(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout,
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        autologon: true,
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: PerformanceFlags::default(),
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None,
        // The pointer is drawn into the picture.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    }
}

/// CredSSP only needs the network for Kerberos, which goes nowhere from
/// the sandbox: NTLM it is.
struct NoNetwork;

impl sspi::network_client::NetworkClient for NoNetwork {
    fn send(&self, _: &sspi::generator::NetworkRequest) -> sspi::Result<Vec<u8>> {
        Err(sspi::Error::new(
            sspi::ErrorKind::NoAuthenticatingAuthority,
            "no Kerberos from the desktop viewer",
        ))
    }
}

fn connector_failure(e: &ironrdp_connector::ConnectorError) -> Failure {
    explain(&e.to_string())
}

/// The window's words for a connection error from IronRDP.
fn explain(text: &str) -> Failure {
    let lower = text.to_lowercase();
    if lower.contains("standard_rdp_security") {
        // xrdp falls back to it when it cannot read its TLS key.
        format!(
            "The RDP server offers only the old RDP security, without TLS, which the viewer \
             does not speak ({text}). On xrdp this usually means it cannot read its TLS key: \
             run sudo adduser xrdp ssl-cert and restart xrdp."
        )
    } else if lower.contains("logon") || lower.contains("credssp") || lower.contains("ntlm") {
        format!("The RDP server did not accept the user name or password ({text}).")
    } else {
        format!("The RDP connection failed: {text}")
    }
}

fn io_failure(e: &io::Error) -> Failure {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        "The RDP server closed the connection.".into()
    } else {
        format!("The RDP connection failed: {e}")
    }
}

/// The pipe under the TLS session.
fn pipe<'f, 'a, W: Write>(framed: &'f mut Framed<Tls<'_, 'a, W>>) -> &'f mut Pipe<'a, W> {
    framed.get_inner_mut().0.sock
}

/// Connects, then runs the session.
fn run<W: Write>(pipe: &mut Pipe<'_, W>, config: ironrdp_connector::Config) -> Result<(), Failure> {
    let client_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    // Display Control lets the desktop follow the pane's size.
    let drdynvc =
        DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector = ClientConnector::new(config, client_addr).with_static_channel(drdynvc);
    let mut framed = Framed::new(&mut *pipe);
    let mut buf = WriteBuf::new();
    while !connector.should_perform_security_upgrade() {
        ironrdp_blocking::single_sequence_step(&mut framed, &mut connector, &mut buf)
            .map_err(|e| connector_failure(&e))?;
    }
    let should_upgrade = ironrdp_blocking::skip_connect_begin(&mut connector);
    let (stream, leftover) = framed.into_inner();
    if !leftover.is_empty() {
        return Err("The RDP server sent data before TLS started.".into());
    }

    let name = pki_types::ServerName::try_from("localhost").map_err(|e| e.to_string())?;
    let tls =
        rustls::ClientConnection::new(tls_config()?, name).map_err(|e| format!("TLS: {e}"))?;
    let mut tls = rustls::StreamOwned::new(tls, stream);
    // Finishes the handshake, which brings the server's certificate.
    tls.flush().map_err(|e| io_failure(&e))?;
    let server_key = tls
        .conn
        .peer_certificates()
        .and_then(|c| c.first())
        .and_then(|cert| server_public_key(cert))
        .ok_or("The RDP server's TLS certificate has no usable key.")?;
    let upgraded = ironrdp_blocking::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = Framed::new(tls);
    let outcome = ironrdp_blocking::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        ServerName::new("localhost"),
        server_key,
        None,
    )
    .map_err(|e| connector_failure(&e))?;
    match outcome {
        ConnectionOutcome::Connected(r) => Active::new(r)?.run(&mut framed),
        ConnectionOutcome::Redirected(_) => Err(
            "The RDP server redirected to another machine, which the viewer does not follow."
                .into(),
        ),
    }
}

/// TLS on ring, accepting the server's certificate as it is (see the
/// module docs) but checking that the server holds its key.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, Failure> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TunnelledServer(provider)))
        .with_no_client_auth();
    // CredSSP does not support resumed TLS sessions.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct TunnelledServer(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for TunnelledServer {
    fn verify_server_cert(
        &self,
        _: &pki_types::CertificateDer<'_>,
        _: &[pki_types::CertificateDer<'_>],
        _: &pki_types::ServerName<'_>,
        _: &[u8],
        _: pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The certificate's public key, which CredSSP binds the login to.
fn server_public_key(cert: &[u8]) -> Option<Vec<u8>> {
    use x509_cert::der::Decode as _;
    let cert = x509_cert::Certificate::from_der(cert).ok()?;
    cert.tbs_certificate()
        .subject_public_key_info()
        .subject_public_key
        .as_bytes()
        .map(<[u8]>::to_vec)
}

fn checked_size(size: DesktopSize) -> Result<(u16, u16), Failure> {
    let (w, h) = (size.width, size.height);
    if w == 0
        || h == 0
        || w > MAX_SIDE
        || h > MAX_SIDE
        || usize::from(w) * usize::from(h) > MAX_PIXELS
    {
        return Err(format!("The RDP server's screen ({w}x{h}) is too large."));
    }
    Ok((w, h))
}

/// The session once connected.
struct Active {
    stage: ActiveStage,
    image: DecodedImage,
    input: Database,
    activation: ironrdp_connector::connection_activation::ConnectionActivationFactory,
    /// Pointer buttons held, as the window reports them.
    buttons: u8,
    /// The size the window was last told.
    told: Option<(u16, u16)>,
    /// The pane's size, which the desktop should take.
    want: Option<(u16, u16)>,
    /// The size last asked of the server, not asked again.
    asked: Option<(u16, u16)>,
}

/// Wheel up and down in [`ToViewer::Pointer`], as VNC has them.
const WHEEL_UP: u8 = 1 << 3;
const WHEEL_DOWN: u8 = 1 << 4;
/// One wheel notch.
const NOTCH: i16 = 120;

impl Active {
    fn new(r: ConnectionResult) -> Result<Self, Failure> {
        if !r
            .input_flags
            .intersects(InputFlags::FASTPATH_INPUT | InputFlags::FASTPATH_INPUT_2)
        {
            return Err(
                "The RDP server does not take fast-path input, which the viewer needs.".into(),
            );
        }
        let (w, h) = checked_size(r.desktop_size)?;
        let stage = ActiveStageBuilder {
            static_channels: r.static_channels,
            user_channel_id: r.user_channel_id,
            io_channel_id: r.io_channel_id,
            message_channel_id: r.message_channel_id,
            share_id: r.share_id,
            compression_type: r.compression_type,
            enable_server_pointer: r.enable_server_pointer,
            pointer_software_rendering: r.pointer_software_rendering,
        }
        .build();
        Ok(Self {
            stage,
            image: DecodedImage::new(PixelFormat::BgrX32, w, h),
            input: Database::new(),
            activation: r.activation_factory,
            buttons: 0,
            told: None,
            want: None,
            asked: Some((w, h)),
        })
    }

    fn run<W: Write>(mut self, framed: &mut Framed<Tls<'_, '_, W>>) -> Result<(), Failure> {
        // Input from before the desktop showed is stale; the pane's
        // newest size is not.
        let early = std::mem::take(&mut pipe(framed).pending);
        self.input_events(
            early
                .into_iter()
                .filter(|m| matches!(m, ToViewer::Resize { .. }))
                .collect(),
        );
        loop {
            self.tell_size(framed)?;
            pipe(framed).interruptible = true;
            let read = framed.read_pdu();
            pipe(framed).interruptible = false;
            match read {
                Ok((action, payload)) => {
                    let at = framed.last_read_at();
                    let outputs = self
                        .stage
                        .process(&mut self.image, action, &payload, at)
                        .map_err(|e| format!("The RDP session failed: {e}"))?;
                    if self.outputs(framed, outputs)? {
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(io_failure(&e)),
            }
            let pending = std::mem::take(&mut pipe(framed).pending);
            let events = self.input_events(pending);
            self.ask_size(framed)?;
            if !events.is_empty() {
                let outputs = self
                    .stage
                    .process_fastpath_input(&mut self.image, &events)
                    .map_err(|e| format!("The RDP session failed: {e}"))?;
                if self.outputs(framed, outputs)? {
                    return Ok(());
                }
            }
        }
    }

    /// Asks the server for the pane's size, over Display Control, once
    /// the server has it open. A server without it keeps its size, and
    /// the window scales the picture.
    fn ask_size<W: Write>(&mut self, framed: &mut Framed<Tls<'_, '_, W>>) -> Result<(), Failure> {
        let Some(want) = self.want else {
            return Ok(());
        };
        if self.asked == Some(want) || self.stage.display_control_ready() != Some(true) {
            return Ok(());
        }
        self.asked = Some(want);
        let (w, h) = want;
        match self
            .stage
            .encode_resize(u32::from(w), u32::from(h), None, None)
        {
            Some(Ok(frame)) => framed.write_all(&frame).map_err(|e| io_failure(&e)),
            Some(Err(e)) => Err(format!("The RDP session failed: {e}")),
            None => Ok(()),
        }
    }

    /// Tells the window the screen's size when it is new.
    fn tell_size<W: Write>(&mut self, framed: &mut Framed<Tls<'_, '_, W>>) -> Result<(), Failure> {
        let size = (self.image.width(), self.image.height());
        if self.told != Some(size) {
            self.told = Some(size);
            let (width, height) = size;
            send(pipe(framed).out, &FromViewer::Size { width, height })
                .map_err(|e| io_failure(&e))?;
        }
        Ok(())
    }

    /// Acts on what the session produced; `true` when it ended.
    fn outputs<W: Write>(
        &mut self,
        framed: &mut Framed<Tls<'_, '_, W>>,
        outputs: Vec<ActiveStageOutput>,
    ) -> Result<bool, Failure> {
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    framed.write_all(&frame).map_err(|e| io_failure(&e))?;
                }
                ActiveStageOutput::GraphicsUpdate(rect) => {
                    self.tell_size(framed)?;
                    self.send_rect(pipe(framed).out, &rect)
                        .map_err(|e| io_failure(&e))?;
                }
                ActiveStageOutput::Terminate(reason) => {
                    return Err(format!("The RDP session ended: {reason}."));
                }
                ActiveStageOutput::DeactivateAll => self.reactivate(framed)?,
                ActiveStageOutput::ServerRedirection(_) => {
                    return Err(
                        "The RDP server redirected to another machine, which the viewer does not \
                         follow."
                            .into(),
                    );
                }
                _ => {}
            }
        }
        Ok(false)
    }

    /// The server reset the display (a new size, or a new session after
    /// the login): run the activation again.
    fn reactivate<W: Write>(&mut self, framed: &mut Framed<Tls<'_, '_, W>>) -> Result<(), Failure> {
        let mut seq = self.activation.create();
        let mut buf = WriteBuf::new();
        loop {
            buf.clear();
            let written = match seq.next_pdu_hint() {
                Some(hint) => {
                    let pdu = framed.read_by_hint(hint).map_err(|e| io_failure(&e))?;
                    seq.step(&pdu, framed.last_read_at(), &mut buf)
                }
                None => seq.step_no_input(&mut buf),
            }
            .map_err(|e| connector_failure(&e))?;
            if let Some(n) = written.size() {
                let reply = buf.filled().get(..n).unwrap_or_default();
                framed.write_all(reply).map_err(|e| io_failure(&e))?;
            }
            match seq.connection_activation_state() {
                ConnectionActivationState::Finalized {
                    desktop_size,
                    share_id,
                    static_channel_chunk_size,
                    enable_server_pointer,
                    pointer_software_rendering,
                    ..
                } => {
                    let (w, h) = checked_size(desktop_size)?;
                    self.image = DecodedImage::new(PixelFormat::BgrX32, w, h);
                    if !self.stage.reactivate(
                        seq.io_channel_id(),
                        seq.user_channel_id(),
                        share_id,
                        enable_server_pointer,
                        pointer_software_rendering,
                        static_channel_chunk_size,
                    ) {
                        return Err("The RDP server reset the display wrongly.".into());
                    }
                    return Ok(());
                }
                ConnectionActivationState::Consumed => {
                    return Err("The RDP server reset the display wrongly.".into());
                }
                _ => {}
            }
        }
    }

    /// Sends the pixels of `rect` (inclusive) to the window.
    fn send_rect(&self, out: &mut impl Write, rect: &InclusiveRectangle) -> io::Result<()> {
        let (w, h) = (self.image.width(), self.image.height());
        if rect.left > rect.right || rect.top > rect.bottom || rect.left >= w || rect.top >= h {
            return Ok(());
        }
        let right = rect.right.min(w - 1);
        let bottom = rect.bottom.min(h - 1);
        let width = right - rect.left + 1;
        let stride = self.image.stride();
        let data = self.image.data();
        let mut pixels =
            Vec::with_capacity(usize::from(width) * usize::from(bottom - rect.top + 1));
        for y in rect.top..=bottom {
            let start = usize::from(y) * stride + usize::from(rect.left) * 4;
            let row = data
                .get(start..start + usize::from(width) * 4)
                .unwrap_or_default();
            pixels.extend(row.chunks_exact(4).map(crate::rgb));
        }
        send_pixels(out, (rect.left, rect.top, width), &pixels)
    }

    /// Turns the window's keyboard and mouse messages into RDP input.
    fn input_events(
        &mut self,
        pending: Vec<ToViewer>,
    ) -> Vec<ironrdp_pdu::input::fast_path::FastPathInputEvent> {
        let mut events = Vec::new();
        for msg in pending {
            match msg {
                ToViewer::Key {
                    scan,
                    extended,
                    down,
                    ..
                } => {
                    let Ok(code) = u8::try_from(scan) else {
                        continue;
                    };
                    if code == 0 {
                        continue;
                    }
                    let key = Scancode::from_u8(extended, code);
                    let op = if down {
                        Operation::KeyPressed(key)
                    } else {
                        Operation::KeyReleased(key)
                    };
                    events.extend(self.input.apply([op]));
                }
                ToViewer::Pointer { buttons, x, y } => {
                    let mut ops = vec![Operation::MouseMove(MousePosition { x, y })];
                    for (bit, button) in [
                        (1, MouseButton::Left),
                        (2, MouseButton::Middle),
                        (4, MouseButton::Right),
                    ] {
                        let (was, is) = (self.buttons & bit != 0, buttons & bit != 0);
                        if is && !was {
                            ops.push(Operation::MouseButtonPressed(button));
                        } else if was && !is {
                            ops.push(Operation::MouseButtonReleased(button));
                        }
                    }
                    for (bit, units) in [(WHEEL_UP, NOTCH), (WHEEL_DOWN, -NOTCH)] {
                        if buttons & bit != 0 && self.buttons & bit == 0 {
                            ops.push(Operation::WheelRotations(WheelRotations {
                                is_vertical: true,
                                rotation_units: units,
                            }));
                        }
                    }
                    self.buttons = buttons;
                    events.extend(self.input.apply(ops));
                }
                ToViewer::Release => {
                    self.buttons = 0;
                    events.extend(self.input.release_all());
                }
                ToViewer::Resize { width, height } => {
                    self.want = Some(desktop_size(width, height));
                }
                // Characters are typed through their keys' scan codes.
                _ => {}
            }
        }
        events
    }
}

/// Feeds `data` to the session as server PDUs after the login, the way
/// the fuzzer does: every update must decode or fail cleanly, and every
/// rectangle sent to the window must fit its framebuffer.
#[doc(hidden)]
pub fn fuzz_server_pdus(data: &[u8]) {
    use crate::framebuffer::Framebuffer;
    use ironrdp_pdu::rdp::client_info::CompressionType;

    let (io, user) = (1003, 1002);
    let size = DesktopSize {
        width: 64,
        height: 48,
    };
    let config = config(String::new(), Zeroizing::new(String::new()), size, 0x409);
    let stage = ActiveStageBuilder {
        static_channels: ironrdp_svc::StaticChannelSet::new(),
        user_channel_id: user,
        io_channel_id: io,
        message_channel_id: None,
        share_id: 0x0001_03ea,
        compression_type: Some(CompressionType::Rdp61),
        enable_server_pointer: true,
        pointer_software_rendering: true,
    }
    .build();
    let mut active = Active {
        stage,
        image: DecodedImage::new(PixelFormat::BgrX32, size.width, size.height),
        input: Database::new(),
        activation: ironrdp_connector::connection_activation::ConnectionActivationFactory::new(
            config, io, user,
        ),
        buttons: 0,
        told: None,
        want: None,
        asked: None,
    };
    let mut fb = Framebuffer::default();
    let _ = fb.apply(FromViewer::Size {
        width: size.width,
        height: size.height,
    });
    let mut rest = data;
    while let Ok(Some(info)) = ironrdp_pdu::find_size(rest) {
        let Some((pdu, next)) = rest.split_at_checked(info.length.max(1)) else {
            return;
        };
        rest = next;
        let Ok(outputs) = active
            .stage
            .process(&mut active.image, info.action, pdu, None)
        else {
            continue;
        };
        for out in outputs {
            if let ActiveStageOutput::GraphicsUpdate(rect) = out {
                let mut sent = Vec::new();
                let _ = active.send_rect(&mut sent, &rect);
                let mut frames = sent.as_slice();
                while let Ok(Some(frame)) = read_frame(&mut frames) {
                    let msg = FromViewer::decode(&frame);
                    assert!(msg.is_some_and(|m| fb.apply(m).is_ok()), "a bad rectangle");
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    //! A real RDP server, when `FEROSSH_TEST_RDP` names one as
    //! "host:port user password" (CI runs xrdp for this): the viewer logs
    //! in and the desktop's pixels reach a framebuffer, with the window's
    //! side relayed by hand.

    use super::*;
    use crate::framebuffer::{Applied, Framebuffer};
    use crate::pipe::{Protocol, write_frame};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[test]
    fn an_xrdp_without_tls_says_how_to_fix_it() {
        let why = explain(
            "[Initiation] reason: client advertised SSL | HYBRID | HYBRID_EX, but server \
             selected STANDARD_RDP_SECURITY",
        );
        assert!(why.contains("adduser xrdp ssl-cert"), "{why}");
    }

    #[test]
    fn logs_in_to_a_real_server() {
        let Ok(spec) = std::env::var("FEROSSH_TEST_RDP") else {
            eprintln!("FEROSSH_TEST_RDP not set; skipped");
            return;
        };
        let mut spec = spec.split_whitespace();
        let (addr, user, password) = (
            spec.next().unwrap().to_owned(),
            spec.next().unwrap().to_owned(),
            spec.next().unwrap().to_owned(),
        );
        let tcp = TcpStream::connect(&addr).unwrap();
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (mut window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || crate::viewer::run(viewer_in, viewer_out));
        let to_viewer = Arc::new(Mutex::new(window_out));
        let tell = |m: ToViewer| write_frame(&mut *to_viewer.lock().unwrap(), &m.encode()).unwrap();
        tell(ToViewer::Start {
            protocol: Protocol::Rdp,
            width: 800,
            height: 600,
            layout: 0x409,
            user: "nobody".into(),
        });
        let mut net = tcp.try_clone().unwrap();
        let pump = to_viewer.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                let n = net.read(&mut buf).unwrap_or(0);
                let msg = if n == 0 {
                    ToViewer::Closed
                } else {
                    ToViewer::Net(buf[..n].to_vec())
                };
                let last = n == 0;
                if write_frame(&mut *pump.lock().unwrap(), &msg.encode()).is_err() || last {
                    return;
                }
            }
        });
        let mut tcp_out = tcp;
        let mut fb = Framebuffer::default();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut drawn = 0usize;
        while Instant::now() < deadline {
            let Some(frame) = read_frame(&mut window_in).unwrap() else {
                panic!("the viewer stopped");
            };
            match fb.apply(FromViewer::decode(&frame).unwrap()).unwrap() {
                Applied::Drawn => drawn += 1,
                Applied::Other(FromViewer::Net(b)) => tcp_out.write_all(&b).unwrap(),
                Applied::Other(FromViewer::NeedPassword) => {
                    tell(ToViewer::User(user.clone()));
                    tell(ToViewer::Password(password.clone().into_bytes()));
                }
                Applied::Other(FromViewer::Failed(why)) => panic!("{why}"),
                Applied::Other(_) => {}
            }
            // The session's xterm has this background.
            if drawn > 0 && fb.pixels().contains(&0x336699) {
                break;
            }
        }
        assert_eq!((fb.width(), fb.height()), (800, 600));
        assert!(fb.pixels().contains(&0x336699), "the desktop never showed");

        // The pane grows: the desktop follows, over Display Control, and
        // is drawn again at the new size.
        tell(ToViewer::Resize {
            width: 1025,
            height: 700,
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "the desktop kept its size");
            let Some(frame) = read_frame(&mut window_in).unwrap() else {
                panic!("the viewer stopped");
            };
            match fb.apply(FromViewer::decode(&frame).unwrap()).unwrap() {
                Applied::Other(FromViewer::Net(b)) => tcp_out.write_all(&b).unwrap(),
                Applied::Other(FromViewer::Failed(why)) => panic!("{why}"),
                _ => {}
            }
            if (fb.width(), fb.height()) == (1024, 700) && fb.pixels().contains(&0x336699) {
                break;
            }
        }

        // Pointer over the xterm (no window manager: focus follows it),
        // then type a command by scan code. On this machine, its file
        // shows that the keys arrived.
        if !addr.starts_with("127.0.0.1:") {
            return;
        }
        let marker = "/tmp/frdp";
        let _ = std::fs::remove_file(marker);
        tell(ToViewer::Pointer {
            buttons: 0,
            x: 100,
            y: 100,
        });
        // touch /tmp/frdp, Enter
        let scans: [u16; 16] = [
            0x14, 0x18, 0x16, 0x2e, 0x23, 0x39, 0x35, 0x14, 0x32, 0x19, 0x35, 0x21, 0x13, 0x20,
            0x19, 0x1c,
        ];
        for scan in scans {
            for down in [true, false] {
                tell(ToViewer::Key {
                    vk: 0,
                    scan,
                    extended: false,
                    down,
                });
            }
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let pump = std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut window_in) {
                if let Some(FromViewer::Net(b)) = FromViewer::decode(&frame)
                    && tcp_out.write_all(&b).is_err()
                {
                    return;
                }
            }
        });
        while !std::path::Path::new(marker).exists() {
            assert!(Instant::now() < deadline, "the typed command never ran");
            std::thread::sleep(Duration::from_millis(50));
        }
        tell(ToViewer::Closed);
        let _ = pump.join();
    }
}
