CREATE DATABASE IF NOT EXISTS Fire_fox_remote_server;
USE Fire_fox_remote_server;

CREATE TABLE IF NOT EXISTS user (
    user_id VARCHAR(255) PRIMARY KEY,
    expire_date DATETIME NOT NULL,
    connections INT NOT NULL DEFAULT 1,
    current_connections INT NOT NULL DEFAULT -1
);

