-- Schema-v3 compatibility fixture: installed OpenSnitch 1.7.2 table layout.
-- https://github.com/evilsocket/opensnitch/tree/v1.7.2/ui/opensnitch/database
PRAGMA user_version=3;
CREATE TABLE connections(time TEXT,node TEXT,action TEXT,protocol TEXT,src_ip TEXT,src_port TEXT,dst_ip TEXT,dst_host TEXT,dst_port TEXT,uid TEXT,pid TEXT,process TEXT,process_args TEXT,process_cwd TEXT,rule TEXT,
 UNIQUE(node,action,protocol,src_ip,src_port,dst_ip,dst_port,uid,pid,process,process_args));
CREATE INDEX time_index ON connections(time);
CREATE TABLE nodes(addr TEXT PRIMARY KEY,hostname TEXT,daemon_version TEXT,daemon_uptime TEXT,daemon_rules TEXT,cons TEXT,cons_dropped TEXT,version TEXT,status TEXT,last_connection TEXT);
CREATE TABLE hosts(what TEXT PRIMARY KEY,hits INTEGER);
CREATE TABLE procs(what TEXT PRIMARY KEY,hits INTEGER);
CREATE TABLE addrs(what TEXT PRIMARY KEY,hits INTEGER);
CREATE TABLE ports(what TEXT PRIMARY KEY,hits INTEGER);
CREATE TABLE users(what TEXT PRIMARY KEY,hits INTEGER);
CREATE TABLE rules(time TEXT,node TEXT,name TEXT,enabled TEXT,precedence TEXT,action TEXT,duration TEXT,operator_type TEXT,operator_sensitive TEXT,operator_operand TEXT,operator_data TEXT,description TEXT,nolog TEXT,created TEXT,UNIQUE(node,name));
