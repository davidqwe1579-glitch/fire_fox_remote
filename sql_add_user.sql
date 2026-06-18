USE Fire_fox_remote_server;

-- 예제 유저 1: 만료일 1년 뒤, 최대 2명 접속 가능, 현재 접속자수 초기값 -1
INSERT INTO user (user_id, expire_date, connections, current_connections) 
VALUES ('test', DATE_ADD(NOW(), INTERVAL 1 YEAR), 2, -1)
ON DUPLICATE KEY UPDATE 
    expire_date = DATE_ADD(NOW(), INTERVAL 1 YEAR),
    connections = 2,
    current_connections = -1;

-- 예제 유저 2: 만료일 무제한(2099년), 최대 5명 접속 가능, 현재 접속자수 초기값 -1
INSERT INTO user (user_id, expire_date, connections, current_connections) 
VALUES ('admin', '2099-12-31 23:59:59', 5, -1)
ON DUPLICATE KEY UPDATE 
    expire_date = '2099-12-31 23:59:59',
    connections = 5,
    current_connections = -1;
