#[macro_use]
extern crate rocket;

use std::fmt;
use std::time::Duration;

use askama_rocket::Template;
use rocket::{fairing::AdHoc, form::Form, http::Status, serde::json::Json, Shutdown, State};
use rocket::fs::{FileServer, relative};
use rocket::response::stream::{Event, EventStream};
use rocket_db_pools::{Connection, Database, sqlx};
use rocket_db_pools::sqlx::{Acquire, Row, sqlite::SqliteRow};
use serde::{Deserialize, Serialize};
use tokio::{select, time};
use tokio::sync::broadcast;

#[launch]
fn rocket() -> _ {
    let (tx, rx) = broadcast::channel::<TodoMutation>(100);
    rocket::build()
        .manage(tx)
        .manage(rx)
        .attach(Todos::init())
        .attach(AdHoc::try_on_ignite("Database Migrations", |rocket| async {
            let pool = match Todos::fetch(&rocket) {
                Some(pool) => pool.0.clone(),
                None => return Err(rocket),
            };
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .expect("Failed to run migrations");
            Ok(rocket)
        }))
        .mount("/", routes![
            index,
            get_todos,
            get_todos_json,
            post_todo,
            post_todo_json,
            delete_todo,
            put_todo,
            put_todo_json,
            todos_stream
        ])
        .mount("/", FileServer::from(relative!("dist")).rank(10))
        .mount("/", FileServer::from(relative!("static")).rank(20))
}

#[derive(Debug, Clone)]
struct TodoError {
    message: String,
}

impl TodoError {
    fn new(message: &str) -> Self {
        TodoError { message: message.to_string() }
    }
}

impl fmt::Display for TodoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Database)]
#[database("sqlite_todos")]
struct Todos(sqlx::SqlitePool);

#[derive(Clone, Serialize, Debug)]
enum TodoMutation {
    Create(Todo),
    Update(Todo),
    Delete(i32),
}

#[derive(Clone, Debug, sqlx::FromRow, Serialize, Deserialize)]
struct Todo {
    id: i32,
    description: String,
    completed: bool,
}

#[derive(FromForm, Serialize, Deserialize)]
struct TodoInput {
    description: String,
}

#[derive(FromForm, Serialize, Deserialize)]
struct TodoUpdate {
    completed: bool,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {}

#[rocket::get("/", format = "text/html")]
fn index() -> IndexTemplate {
    IndexTemplate {}
}

#[derive(Template)]
#[template(path = "todos.html")]
struct TodosTemplate {
    todos: Vec<Todo>,
}

#[derive(Template)]
#[template(path = "todo.html")]
struct NewTodoTemplate {
    todo: Todo,
}

#[rocket::get("/todos", format = "text/html")]
async fn get_todos(pool: Connection<Todos>) -> TodosTemplate {
    TodosTemplate { todos: fetch_todos(pool).await.expect("Unable to fetch todos") }
}

#[rocket::get("/todos", format = "application/json", rank = 2)]
async fn get_todos_json(pool: Connection<Todos>) -> Json<Vec<Todo>> {
    Json(fetch_todos(pool).await.expect("Unable to fetch todos"))
}

async fn fetch_todos(mut pool: Connection<Todos>) -> Result<Vec<Todo>, TodoError> {
    let conn = pool.acquire().await
        .map_err(|e| TodoError::new(format!("unable to acquire connection: {e:?}").as_str()))?;
    sqlx::query("SELECT * FROM todos")
        .map(|row: SqliteRow| Todo {
            id: row.get(0),
            description: row.get(1),
            completed: row.get(2),
        })
        .fetch_all(conn)
        .await
        .map_err(|e| TodoError::new(format!("unable to fetch todos: {e:?}").as_str()))
}

#[rocket::delete("/todos/<id>")]
async fn delete_todo(id: i32, mut pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> Status {
    if let Ok(conn) = pool.acquire().await {
        sqlx::query("DELETE FROM todos WHERE id = ?")
            .bind(id)
            .execute(conn)
            .await
            .expect("Failed to delete todo");
    }
    tx.send(TodoMutation::Delete(id))
        .expect("Failed to send mutation");
    Status::Ok
}

#[rocket::put("/todos/<id>", data = "<todo_update>", format = "application/json", rank = 2)]
async fn put_todo_json(id: i32, todo_update: Json<TodoUpdate>, pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> Json<Todo> {
    Json(update_todo(&id, todo_update.into_inner(), pool, tx).await.expect("Unable to update todo"))
}

#[rocket::put("/todos/<id>", data = "<todo_update>", format = "application/x-www-form-urlencoded")]
async fn put_todo(id: i32, todo_update: Form<TodoUpdate>, pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> NewTodoTemplate {
    NewTodoTemplate { todo: update_todo(&id, todo_update.into_inner(), pool, tx).await.expect("Unable to update todo") }
}

async fn update_todo(id: &i32, todo_update: TodoUpdate, mut pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> Result<Todo, TodoError> {
    let conn = pool.acquire().await
        .map_err(|e| TodoError::new(format!("unable to acquire connection: {e:?}").as_str()))?;
    sqlx::query("UPDATE todos SET completed = ? WHERE id = ?")
        .bind(todo_update.completed)
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(|e| TodoError::new(format!("unable to update todo: {e:?}").as_str()))?;
    let todo = sqlx::query("SELECT * FROM todos WHERE id = ?")
        .bind(id)
        .map(|row: SqliteRow| Todo {
            id: row.get(0),
            description: row.get(1),
            completed: row.get(2),
        })
        .fetch_one(conn)
        .await
        .map_err(|e| TodoError::new(format!("unable to fetch todo: {e:?}").as_str()))?;
    tx.send(TodoMutation::Update(todo.clone()))
        .map_err(|e| TodoError::new(format!("unable to send mutation: {e:?}").as_str()))?;
    Ok(todo)
}

#[rocket::post("/todos", data = "<todo_input>", format = "application/x-www-form-urlencoded")]
async fn post_todo(todo_input: Form<TodoInput>, pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> NewTodoTemplate {
    NewTodoTemplate { todo: create_todo(todo_input.into_inner(), pool, tx).await.expect("Unable to create todo") }
}

#[rocket::post("/todos", data = "<todo_input>", format = "application/json", rank = 2)]
async fn post_todo_json(todo_input: Json<TodoInput>, pool: Connection<Todos>, tx: &State<broadcast::Sender<TodoMutation>>) -> Json<Todo> {
    Json(create_todo(todo_input.into_inner(), pool, tx).await.expect("Unable to create todo"))
}

async fn create_todo(todo_input: TodoInput, mut pool: Connection<Todos>, tx: &broadcast::Sender<TodoMutation>) -> Result<Todo, TodoError> {
    let conn = pool.acquire().await
        .map_err(|e| TodoError::new(format!("unable to acquire connection: {e:?}").as_str()))?;
    sqlx::query("INSERT INTO todos (description) VALUES (?)")
        .bind(todo_input.description.clone())
        .execute(&mut *conn)
        .await
        .map_err(|e| TodoError::new(format!("unable to create todo: {e:?}").as_str()))?;
    let todo = sqlx::query("SELECT * FROM todos WHERE id = last_insert_rowid()")
        .map(|row: SqliteRow| Todo {
            id: row.get(0),
            description: row.get(1),
            completed: row.get(2),
        })
        .fetch_one(conn)
        .await
        .map_err(|e| TodoError::new(format!("unable to fetch todo: {e:?}").as_str()))?;
    tx.send(TodoMutation::Create(todo.clone()))
        .map_err(|e| TodoError::new(format!("unable to send mutation: {e:?}").as_str()))?;
    Ok(todo)
}
#[get("/todos/stream")]
async fn todos_stream(mut shutdown: Shutdown, tx: &State<broadcast::Receiver<TodoMutation>>) -> EventStream![Event] {
    let mut rx = (*tx.inner()).resubscribe();
    let mut interval = time::interval(Duration::from_secs(5));
    EventStream! {
        loop {
            select! {
                Ok(mutation) = rx.recv() => {
                    yield match mutation {
                        TodoMutation::Create(todo) => Event::data(NewTodoTemplate{
                                todo: todo.clone()
                        }.render().unwrap())
                            .id(todo.id.to_string())
                            .event("create"),
                        TodoMutation::Update(todo) => Event::data(NewTodoTemplate{
                                todo: todo.clone()
                        }.render().unwrap())
                            .id(todo.id.to_string())
                            .event(format!("update-{}", todo.id)),
                        TodoMutation::Delete(id) => Event::data(id.to_string())
                            .id(id.to_string())
                            .event("delete"),
                    };
                },
                _ = interval.tick() => yield Event::json(&"ping").event("ping"),
                _ = &mut shutdown => break,
            }
        }
    }
}
