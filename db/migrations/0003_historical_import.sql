ALTER TABLE `sessions` ADD `is_imported` integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE `sessions` ADD `native_session_id` text;--> statement-breakpoint
CREATE INDEX `sessions_by_native_id` ON `sessions` (`provider`,`native_session_id`);
