CREATE TABLE `session_details` (
	`session_id` text PRIMARY KEY NOT NULL,
	`data` text NOT NULL
);
--> statement-breakpoint
-- Hand-written: move the transcripts across before the column goes away.
INSERT INTO `session_details` (`session_id`, `data`) SELECT `id`, `data` FROM `sessions`;--> statement-breakpoint
DROP INDEX `sessions_list`;--> statement-breakpoint
CREATE INDEX `sessions_by_updated_at` ON `sessions` (`updated_at`);--> statement-breakpoint
ALTER TABLE `sessions` DROP COLUMN `data`;