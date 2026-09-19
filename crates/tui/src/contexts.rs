//! The Contexts screen: rows from the `Contexts` seam, an add form, a login field. It owns
//! its input, like the other modals; the app owns everything a key needs to reach - the seam,
//! the session, the mode.

use nutsh_core::contexts::{ConnectRequest, ContextRow, ENV_ROW, NewContext};

use crate::key::Key;
use crate::text::{Edit, Input};

/// What a key on the screen, the form, or the login field asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The screen dealt with the key itself - a cursor move, an edited field, a key that
    /// means nothing here. A redraw is the whole effect.
    Handled,
    /// The add form or the login field opened; the app switches to `Mode::Form`.
    Opened,
    /// The add form or the login field closed; back to the rows.
    Closed,
    /// Re-read the rows through the seam.
    Reload,
    /// Remove this context, then re-read the rows.
    Remove(String),
    /// Start this connect. The second field is the host, for the message under the rows.
    Connect(ConnectRequest, String),
    /// `space`: read this context beside the session, or stop reading it.
    Toggle(String),
    /// `esc` on the rows: back to the table, when there is one.
    Back,
    Quit,
    /// `:` - only the app knows whether there is a session to run a command against.
    Palette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Host,
    Port,
    Username,
    Password,
    Cluster,
    Insecure,
    Readonly,
}

pub const FIELDS: [Field; 8] = [
    Field::Name,
    Field::Host,
    Field::Port,
    Field::Username,
    Field::Password,
    Field::Cluster,
    Field::Insecure,
    Field::Readonly,
];

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Host => "host",
            Field::Port => "port",
            Field::Username => "username",
            Field::Password => "password",
            Field::Cluster => "cluster",
            Field::Insecure => "insecure",
            Field::Readonly => "read-only",
        }
    }
}

#[derive(Default, Clone)]
pub struct Form {
    pub name: Input,
    pub host: Input,
    pub port: Input,
    pub username: Input,
    pub password: Input,
    pub cluster: Input,
    pub insecure: bool,
    pub readonly: bool,
    pub field: usize,
    pub error: Option<String>,
}

/// By hand, like `NewContext` in the core crate: a derived `Debug` would print the password
/// into whatever log or panic message formatted the form.
impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Form")
            .field("name", &self.name.as_str())
            .field("host", &self.host.as_str())
            .field("port", &self.port.as_str())
            .field("username", &self.username.as_str())
            .field("password", &"***")
            .field("cluster", &self.cluster.as_str())
            .field("insecure", &self.insecure)
            .field("readonly", &self.readonly)
            .field("field", &self.field)
            .field("error", &self.error)
            .finish()
    }
}

impl Form {
    pub fn new() -> Form {
        Form {
            port: crate::DEFAULT_PORT.to_string().into(),
            ..Form::default()
        }
    }

    /// The field the cursor is on. `field` is only ever moved modulo `FIELDS`, so the fallback
    /// is unreachable; it is here so that a drawing bug cannot become a panic.
    pub fn current(&self) -> Field {
        FIELDS.get(self.field).copied().unwrap_or(Field::Name)
    }

    pub fn text_mut(&mut self) -> Option<&mut Input> {
        match self.current() {
            Field::Name => Some(&mut self.name),
            Field::Host => Some(&mut self.host),
            Field::Port => Some(&mut self.port),
            Field::Username => Some(&mut self.username),
            Field::Password => Some(&mut self.password),
            Field::Cluster => Some(&mut self.cluster),
            Field::Insecure | Field::Readonly => None,
        }
    }

    /// The same split, borrowed immutably: the drawing needs the cursor of the field the keys go
    /// to, and a `&mut` accessor cannot be called from a `&self` draw.
    pub fn text(&self) -> Option<&Input> {
        match self.current() {
            Field::Name => Some(&self.name),
            Field::Host => Some(&self.host),
            Field::Port => Some(&self.port),
            Field::Username => Some(&self.username),
            Field::Password => Some(&self.password),
            Field::Cluster => Some(&self.cluster),
            Field::Insecure | Field::Readonly => None,
        }
    }

    /// Where the block goes on the field the keys go to; `None` for a checkbox, which takes none
    /// because `space` toggles it. The password's is in **characters**, because the value drawn
    /// for it is one `*` per character rather than the text itself.
    pub fn cursor(&self) -> Option<usize> {
        let input = self.text()?;
        Some(match self.current() {
            Field::Password => input.as_str()[..input.cursor()].chars().count(),
            _ => input.cursor(),
        })
    }

    pub fn toggle(&mut self) {
        match self.current() {
            Field::Insecure => self.insecure = !self.insecure,
            Field::Readonly => self.readonly = !self.readonly,
            _ => {}
        }
    }

    /// The value each field shows; the password only as its length.
    pub fn value(&self, field: Field) -> String {
        match field {
            Field::Name => self.name.as_str().to_string(),
            Field::Host => self.host.as_str().to_string(),
            Field::Port => self.port.as_str().to_string(),
            Field::Username => self.username.as_str().to_string(),
            Field::Password => mask(&self.password),
            Field::Cluster => self.cluster.as_str().to_string(),
            Field::Insecure => checkbox(self.insecure),
            Field::Readonly => checkbox(self.readonly),
        }
    }

    /// The request, or the first validation error. `taken` says whether a name exists.
    pub fn submit(&self, taken: impl Fn(&str) -> bool) -> Result<NewContext, String> {
        let name = self.name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err("name: letters, digits, - _ . only".into());
        }
        if taken(name) {
            return Err(format!("context {name} already exists"));
        }
        if self.host.trim().is_empty() {
            return Err("host is required".into());
        }
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| "port must be a number".to_string())?;
        if self.username.trim().is_empty() {
            return Err("username is required".into());
        }
        if self.password.is_empty() {
            return Err("password is required".into());
        }
        Ok(NewContext {
            name: name.to_string(),
            host: self.host.trim().to_string(),
            port,
            username: self.username.trim().to_string(),
            password: self.password.as_str().to_string(),
            cluster: Some(self.cluster.trim().to_string()).filter(|c| !c.is_empty()),
            // A bundle is a path to a file, which is a file picker's job rather than a text
            // field's; `nutsh ctx add --ca-bundle` is where one is set for now.
            ca_bundle: None,
            insecure: self.insecure,
            readonly: self.readonly,
        })
    }
}

/// The password prompt for one row.
#[derive(Clone)]
pub struct Login {
    pub name: String,
    pub password: Input,
    pub error: Option<String>,
}

/// By hand, for the same reason as [`Form`].
impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("name", &self.name)
            .field("password", &"***")
            .field("error", &self.error)
            .finish()
    }
}

impl Login {
    pub fn request(&self) -> ConnectRequest {
        request(&self.name, Some(self.password.as_str().to_string()))
    }

    /// Where the block goes in the masked value: in **characters**, because the mask is one `*`
    /// per character rather than the password itself.
    pub fn cursor(&self) -> usize {
        self.password.as_str()[..self.password.cursor()]
            .chars()
            .count()
    }
}

/// The box over the rows while there is one. One field rather than two options, because there
/// is never both: which of them has the keys, the error and the connect is then structural.
#[derive(Debug)]
pub enum Prompt {
    Add(Form),
    Login(Login),
}

impl Prompt {
    /// What the connect it asked for came back with, shown under its fields.
    pub fn set_error(&mut self, error: String) {
        match self {
            Prompt::Add(form) => form.error = Some(error),
            Prompt::Login(login) => login.error = Some(error),
        }
    }
}

/// What `PgUp` and `PgDn` move by. The screen does not know how tall the terminal is - it owns
/// the rows, not the frame - so it steps by the same fixed page as a table does.
const PAGE: usize = 20;

#[derive(Debug, Default)]
pub struct Screen {
    pub rows: Vec<ContextRow>,
    pub selected: usize,
    pub prompt: Option<Prompt>,
    /// What the last action had to say; a reload clears it.
    pub message: Option<String>,
    /// Why there are no rows, when reading them is what failed. Separate from `message` so
    /// that "your config file is broken" does not read as "you have no contexts yet".
    pub list_error: Option<String>,
    pub pending: bool,
    pub confirm_remove: Option<String>,
    /// The contexts joined beside the session (`:ctx a b`, or `space` here), drawn with `+`.
    pub joined: Vec<String>,
}

impl Screen {
    pub fn current(&self) -> Option<&ContextRow> {
        self.rows.get(self.selected)
    }

    /// Put the cursor on `name` when it is there; leave it alone when it is not.
    pub fn select(&mut self, name: &str) {
        if let Some(i) = self.rows.iter().position(|r| r.name == name) {
            self.selected = i;
        }
    }

    /// The password prompt for `name`, with the reason it was opened when there is one. The
    /// reason goes under the field rather than under the rows, which the box covers.
    pub fn open_login(&mut self, name: String, error: Option<String>) {
        self.prompt = Some(Prompt::Login(Login {
            name,
            password: Input::default(),
            error,
        }));
    }

    /// A key on the rows. While a connect runs nothing is accepted: the screen is about to be
    /// replaced either way, and `ctrl-c` is handled before the mode dispatch.
    pub fn key(&mut self, key: Key) -> Action {
        if self.pending {
            return Action::Handled;
        }
        // A confirmation swallows the next key whatever it is: only `y` removes.
        if let Some(name) = self.confirm_remove.take() {
            return if key == Key::Char('y') {
                Action::Remove(name)
            } else {
                Action::Handled
            };
        }
        let last = self.rows.len().saturating_sub(1);
        match key {
            Key::Char('j') | Key::Down => {
                self.selected = (self.selected + 1).min(last);
                Action::Handled
            }
            Key::Char('k') | Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Action::Handled
            }
            Key::Char('g') | Key::Home => {
                self.selected = 0;
                Action::Handled
            }
            Key::Char('G') | Key::End => {
                self.selected = last;
                Action::Handled
            }
            Key::PageDown => {
                self.selected = (self.selected + PAGE).min(last);
                Action::Handled
            }
            Key::PageUp => {
                self.selected = self.selected.saturating_sub(PAGE);
                Action::Handled
            }
            Key::Ctrl('r') => Action::Reload,
            Key::Char('a') => {
                self.prompt = Some(Prompt::Add(Form::new()));
                Action::Opened
            }
            Key::Char('l') => match self.current() {
                Some(r) => {
                    let name = r.name.clone();
                    self.open_login(name, None);
                    Action::Opened
                }
                None => Action::Handled,
            },
            Key::Char('d') => match self.current() {
                // `(env)` is not in the config file, so there is nothing to remove.
                Some(r) if r.name == ENV_ROW => {
                    self.message = Some("the environment target is not saved".into());
                    Action::Handled
                }
                Some(r) => {
                    self.confirm_remove = Some(r.name.clone());
                    Action::Handled
                }
                None => Action::Handled,
            },
            Key::Enter => match self.current().cloned() {
                Some(r) if r.has_password => Action::Connect(request(&r.name, None), r.host),
                Some(r) => {
                    self.open_login(r.name, None);
                    Action::Opened
                }
                None => Action::Handled,
            },
            Key::Char(' ') => match self.current() {
                Some(r) if r.name == ENV_ROW => {
                    self.message = Some("the environment target cannot be joined".into());
                    Action::Handled
                }
                Some(r) if r.current => {
                    self.message = Some(format!("{} is the session", r.name));
                    Action::Handled
                }
                Some(r) => Action::Toggle(r.name.clone()),
                None => Action::Handled,
            },
            Key::Esc => Action::Back,
            Key::Char('q') => Action::Quit,
            Key::Char(':') => Action::Palette,
            _ => Action::Handled,
        }
    }

    /// A key on the open box. The login field is the add form with only its password row, so
    /// they share the mode and this entry point. The box stays open while its connect runs - a
    /// rejected password must not cost the user eight typed fields - so it ignores keys the
    /// same way the rows do, or `enter` would start a second connect.
    pub fn form_key(&mut self, key: Key) -> Action {
        if self.pending {
            return Action::Handled;
        }
        match self.prompt {
            Some(Prompt::Add(_)) => self.add_key(key),
            Some(Prompt::Login(_)) => self.login_key(key),
            None => Action::Closed,
        }
    }

    fn add_key(&mut self, key: Key) -> Action {
        let Some(Prompt::Add(form)) = self.prompt.as_mut() else {
            return Action::Closed;
        };
        match key {
            Key::Esc => {
                self.prompt = None;
                Action::Closed
            }
            Key::Tab | Key::Down => {
                form.field = (form.field + 1) % FIELDS.len();
                Action::Handled
            }
            Key::BackTab | Key::Up => {
                form.field = (form.field + FIELDS.len() - 1) % FIELDS.len();
                Action::Handled
            }
            // An edit answers whatever the last submit complained about, so the complaint goes
            // with it rather than sitting under a form that has since been fixed.
            Key::Char(' ') if form.text_mut().is_none() => {
                form.toggle();
                form.error = None;
                Action::Handled
            }
            Key::Enter => {
                let names = &self.rows;
                match form.submit(|n| names.iter().any(|r| r.name == n)) {
                    Err(e) => {
                        form.error = Some(e);
                        Action::Handled
                    }
                    Ok(new) => {
                        let host = new.host.clone();
                        Action::Connect(ConnectRequest::Add(new), host)
                    }
                }
            }
            // Every editing key, from the one implementation `text.rs` owns. A checkbox has no
            // text, so an editing key on one is a no-op rather than a special case - and a key
            // `Input` does not own comes back `None` and is swallowed, exactly as the old
            // catch-all swallowed it.
            key => {
                let edit = form.text_mut().and_then(|text| text.key(key));
                if edit == Some(Edit::Changed) {
                    form.error = None;
                }
                Action::Handled
            }
        }
    }

    fn login_key(&mut self, key: Key) -> Action {
        let Some(Prompt::Login(login)) = self.prompt.as_mut() else {
            return Action::Closed;
        };
        match key {
            Key::Esc => {
                self.prompt = None;
                Action::Closed
            }
            Key::Enter if login.password.is_empty() => {
                login.error = Some("password is required".into());
                Action::Handled
            }
            Key::Enter => {
                let request = login.request();
                let host = host_of(&self.rows, &login.name);
                Action::Connect(request, host)
            }
            // The same one key map, on its one field.
            key => {
                if login.password.key(key) == Some(Edit::Changed) {
                    login.error = None;
                }
                Action::Handled
            }
        }
    }
}

/// The connect for a row: with the password the user typed, or with whatever is stored for it.
/// `(env)` is not a saved context, so both of its answers come from the environment's row.
pub fn request(name: &str, password: Option<String>) -> ConnectRequest {
    match (name == ENV_ROW, password) {
        (true, password) => ConnectRequest::Env { password },
        (false, Some(password)) => ConnectRequest::Login {
            name: name.to_string(),
            password,
        },
        (false, None) => ConnectRequest::Stored {
            name: name.to_string(),
        },
    }
}

/// The host of a row by name, for the `connecting to HOST…` message.
fn host_of(rows: &[ContextRow], name: &str) -> String {
    rows.iter()
        .find(|r| r.name == name)
        .map(|r| r.host.clone())
        .unwrap_or_default()
}

/// A password as its length: what the form and the login field show of it.
pub fn mask(password: &str) -> String {
    "*".repeat(password.chars().count())
}

fn checkbox(on: bool) -> String {
    (if on { "[x]" } else { "[ ]" }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, has_password: bool) -> ContextRow {
        ContextRow {
            name: name.into(),
            host: "pc.lab".into(),
            port: crate::DEFAULT_PORT,
            username: "admin".into(),
            cluster: None,
            readonly: false,
            insecure: false,
            current: false,
            has_password,
        }
    }

    fn filled() -> Form {
        Form {
            name: "lab".into(),
            host: "pc.lab".into(),
            port: crate::DEFAULT_PORT.to_string().into(),
            username: "admin".into(),
            password: "hunter2".into(),
            ..Form::new()
        }
    }

    #[test]
    fn the_form_reports_the_first_thing_wrong_with_it() {
        let free = |_: &str| false;
        assert_eq!(
            Form::new().submit(free),
            Err("name: letters, digits, - _ . only".into())
        );
        assert_eq!(
            filled().submit(|n| n == "lab"),
            Err("context lab already exists".into())
        );
        let mut form = filled();
        form.host = Input::default();
        assert_eq!(form.submit(free), Err("host is required".into()));
        let mut form = filled();
        form.port = "abc".into();
        assert_eq!(form.submit(free), Err("port must be a number".into()));
        let mut form = filled();
        form.username = Input::default();
        assert_eq!(form.submit(free), Err("username is required".into()));
        let mut form = filled();
        form.password = Input::default();
        assert_eq!(form.submit(free), Err("password is required".into()));
    }

    /// A trimmed name, a cluster that is empty rather than `Some("")`, and no bundle.
    #[test]
    fn a_valid_form_becomes_a_new_context() {
        let mut form = filled();
        form.name = " lab-1 ".into();
        form.insecure = true;
        let new = form.submit(|_| false).expect("valid");
        assert_eq!(new.name, "lab-1");
        assert_eq!(new.port, crate::DEFAULT_PORT);
        assert_eq!(new.cluster, None);
        assert_eq!(new.ca_bundle, None);
        assert!(new.insecure && !new.readonly);
    }

    /// Only `y` removes; every other key cancels, and none of them falls through to the row
    /// underneath (`d` on a confirmation must not open a second one).
    #[test]
    fn the_remove_confirmation_takes_exactly_one_key() {
        let mut screen = Screen {
            rows: vec![row("lab", true)],
            ..Screen::default()
        };
        assert_eq!(screen.key(Key::Char('d')), Action::Handled);
        assert_eq!(screen.confirm_remove.as_deref(), Some("lab"));
        assert_eq!(screen.key(Key::Char('d')), Action::Handled);
        assert_eq!(screen.confirm_remove, None);
        screen.key(Key::Char('d'));
        assert_eq!(screen.key(Key::Char('y')), Action::Remove("lab".into()));
        assert_eq!(screen.confirm_remove, None);
    }

    /// Nothing that formats a form or a login field may spell out the password: a panic
    /// message, a log line, and `{:?}` in a debugger all go through `Debug`. Not even its
    /// length, which is why the stand-in is fixed rather than the mask the screen draws.
    #[test]
    fn debug_never_prints_a_password() {
        let form = format!("{:?}", filled());
        assert!(!form.contains("hunter2"), "{form}");
        assert!(form.contains("***") && !form.contains("****"), "{form}");
        let login = format!(
            "{:?}",
            Login {
                name: "lab".into(),
                password: "hunter2".into(),
                error: None,
            }
        );
        assert!(!login.contains("hunter2"), "{login}");
        assert!(login.contains("***") && !login.contains("****"), "{login}");
        let screen = format!(
            "{:?}",
            Screen {
                prompt: Some(Prompt::Add(filled())),
                ..Screen::default()
            }
        );
        assert!(!screen.contains("hunter2"), "{screen}");
    }

    /// The four requests a row can ask for, by whether it is `(env)` and whether a password
    /// was typed.
    #[test]
    fn a_request_says_where_the_password_comes_from() {
        assert_eq!(
            request("lab", None),
            ConnectRequest::Stored { name: "lab".into() }
        );
        assert_eq!(
            request("lab", Some("hunter2".into())),
            ConnectRequest::Login {
                name: "lab".into(),
                password: "hunter2".into(),
            }
        );
        assert_eq!(
            request(ENV_ROW, None),
            ConnectRequest::Env { password: None }
        );
        let typed = Login {
            name: ENV_ROW.into(),
            password: "hunter2".into(),
            error: None,
        };
        assert_eq!(
            typed.request(),
            ConnectRequest::Env {
                password: Some("hunter2".into()),
            }
        );
    }

    /// The `(env)` row is a target, not a saved context: it is never removed, and its stored
    /// password is the environment's.
    #[test]
    fn the_env_row_is_not_a_saved_context() {
        let mut screen = Screen {
            rows: vec![row(ENV_ROW, true)],
            ..Screen::default()
        };
        assert_eq!(screen.key(Key::Char('d')), Action::Handled);
        assert_eq!(screen.confirm_remove, None);
        assert!(screen.message.is_some());
        assert_eq!(
            screen.key(Key::Enter),
            Action::Connect(ConnectRequest::Env { password: None }, "pc.lab".into())
        );
    }

    /// A list taller than the screen needs more than `j` and `k` to get about it, and the
    /// same keys the table uses are the ones the user will try.
    #[test]
    fn the_rows_jump_by_page_and_to_the_ends() {
        let mut screen = Screen {
            rows: (0..30).map(|i| row(&format!("ctx{i:02}"), true)).collect(),
            ..Screen::default()
        };
        assert_eq!(screen.key(Key::Char('G')), Action::Handled);
        assert_eq!(screen.selected, 29);
        screen.key(Key::Char('g'));
        assert_eq!(screen.selected, 0);
        screen.key(Key::PageDown);
        assert_eq!(screen.selected, PAGE);
        screen.key(Key::PageDown);
        assert_eq!(screen.selected, 29, "and no further than the last row");
        screen.key(Key::PageUp);
        assert_eq!(screen.selected, 29 - PAGE);
        screen.key(Key::PageUp);
        assert_eq!(screen.selected, 0, "nor before the first");
        screen.key(Key::End);
        assert_eq!(screen.selected, 29);
        screen.key(Key::Home);
        assert_eq!(screen.selected, 0);
    }

    /// No rows: navigation and the keys that act on a row do nothing rather than panic.
    #[test]
    fn an_empty_screen_has_nothing_to_act_on() {
        let mut screen = Screen::default();
        for key in [
            Key::Char('j'),
            Key::Char('k'),
            Key::Enter,
            Key::Char('l'),
            Key::Char('d'),
        ] {
            assert_eq!(screen.key(key), Action::Handled, "{key:?}");
        }
        assert_eq!(screen.selected, 0);
        assert!(screen.prompt.is_none() && screen.confirm_remove.is_none());
    }

    /// Both the rows and the open box: a second `enter` while a connect runs would start a
    /// second one, and the typed fields must survive untouched until the first comes back.
    #[test]
    fn a_connect_in_flight_swallows_every_key() {
        let mut screen = Screen {
            rows: vec![row("lab", true)],
            pending: true,
            ..Screen::default()
        };
        assert_eq!(screen.key(Key::Char('a')), Action::Handled);
        assert_eq!(screen.key(Key::Enter), Action::Handled);
        assert!(screen.prompt.is_none());
        screen.prompt = Some(Prompt::Add(filled()));
        assert_eq!(screen.form_key(Key::Enter), Action::Handled);
        assert_eq!(screen.form_key(Key::Backspace), Action::Handled);
        let Some(Prompt::Add(form)) = screen.prompt.as_ref() else {
            panic!("the form is kept")
        };
        assert_eq!(form.password.as_str(), "hunter2");
    }
}
