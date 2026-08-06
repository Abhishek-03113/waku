CREATE TABLE `messages` (
	`id` text PRIMARY KEY NOT NULL,
	`session_id` text NOT NULL,
	`turn_id` text,
	`position` integer NOT NULL,
	`role` text NOT NULL,
	`content` text NOT NULL,
	`created_at` integer NOT NULL,
	`streaming` integer NOT NULL
);
--> statement-breakpoint
CREATE INDEX `messages_by_session` ON `messages` (`session_id`,`position`);--> statement-breakpoint
-- Hand-written: move messages out of the session JSON and into their own rows.
-- json_each over an array yields the element index as `key`, which is the
-- conversation order we want to preserve.
INSERT OR IGNORE INTO `messages` (`id`, `session_id`, `turn_id`, `position`, `role`, `content`, `created_at`, `streaming`)
SELECT
	json_extract(message.value, '$.id'),
	session.id,
	json_extract(message.value, '$.turn_id'),
	message.key,
	json_extract(message.value, '$.role'),
	json_extract(message.value, '$.content'),
	json_extract(message.value, '$.created_at'),
	CASE WHEN json_extract(message.value, '$.streaming') THEN 1 ELSE 0 END
FROM `sessions` AS session, json_each(session.data, '$.messages') AS message
WHERE json_type(session.data, '$.messages') = 'array';--> statement-breakpoint
UPDATE `sessions` SET `data` = json_remove(`data`, '$.messages') WHERE json_type(`data`, '$.messages') = 'array';