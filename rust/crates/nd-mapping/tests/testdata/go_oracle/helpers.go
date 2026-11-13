package main

import (
	"io/fs"
	"time"
)

func seconds(v any) time.Duration {
	f := v.(float64)
	return time.Duration(f * float64(time.Second))
}

func asInt(v any) int {
	return int(v.(float64))
}

func fs_Stat(fsys fs.FS, name string) (int64, error) {
	fi, err := fs.Stat(fsys, name)
	if err != nil {
		return 0, err
	}
	return fi.Size(), nil
}

type stubFileInfo struct{}

func (stubFileInfo) Name() string         { return "test.mp3" }
func (stubFileInfo) Size() int64          { return 0 }
func (stubFileInfo) Mode() fs.FileMode    { return 0o644 }
func (stubFileInfo) ModTime() time.Time   { return time.Unix(0, 0) }
func (stubFileInfo) IsDir() bool          { return false }
func (stubFileInfo) Sys() any             { return nil }
func (stubFileInfo) BirthTime() time.Time { return time.Unix(0, 0) }
