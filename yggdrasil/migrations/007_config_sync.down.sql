-- Sprint 039.1: Rollback config sync + version tracking tables

DROP INDEX IF EXISTS yggdrasil.idx_config_files_type_project;
DROP TABLE IF EXISTS yggdrasil.config_files;
DROP TABLE IF EXISTS yggdrasil.version_info;
