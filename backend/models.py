import uuid
import secrets
from datetime import datetime, timezone
from sqlalchemy import Column, String, Text, DateTime, Integer, ForeignKey, Boolean, UniqueConstraint
from sqlalchemy.orm import DeclarativeBase, relationship


class Base(DeclarativeBase):
    pass


def new_submission_key() -> str:
    """256-bit submission key from the OS CSPRNG (secrets -> os.urandom).
    Not derived from time or any seed; brute force is infeasible (2^256)."""
    return secrets.token_hex(32)  # 64 hex chars; fills the String(64) column


class Snippet(Base):
    __tablename__ = "snippets"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    title = Column(String(255), default="Untitled")
    local_path = Column(Text, nullable=True)  # if set, project root is this folder (for Drive sync)
    code = Column(Text, nullable=True)  # kept for backward compat; files preferred
    language = Column(String(50), default="cpp")
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))
    updated_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc), onupdate=lambda: datetime.now(timezone.utc))
    version = Column(Integer, default=1)
    cpp_standard = Column(String(10), default="c++17")
    deleted_at = Column(DateTime(timezone=True), nullable=True)  # set => in trash


class Class(Base):
    __tablename__ = "classes"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    name = Column(String(255), nullable=False)
    course = Column(String(50), nullable=False)
    cohort = Column(String(20), nullable=False)
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))

    students = relationship("Student", back_populates="klass", cascade="all, delete-orphan")
    assignments = relationship("Assignment", back_populates="klass", cascade="all, delete-orphan")


class Student(Base):
    __tablename__ = "students"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    class_id = Column(String(36), ForeignKey("classes.id", ondelete="CASCADE"), nullable=False)
    serial = Column(Integer, nullable=True)  # from import; never auto-generated
    name = Column(String(255), nullable=False)
    email = Column(String(255), nullable=True)
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))

    klass = relationship("Class", back_populates="students")


class Assignment(Base):
    __tablename__ = "assignments"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    class_id = Column(String(36), ForeignKey("classes.id", ondelete="CASCADE"), nullable=False)
    name = Column(String(255), nullable=False)
    slot = Column(Integer, nullable=False)
    root_folder = Column(Text, nullable=True)  # assignment root folder (workspace)
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))

    klass = relationship("Class", back_populates="assignments")


class SubmissionKey(Base):
    __tablename__ = "submission_keys"

    key = Column(String(64), primary_key=True, default=new_submission_key)
    student_name = Column(String(255), nullable=False)
    course = Column(String(50), nullable=False)
    cohort = Column(String(20), nullable=False)
    slot = Column(Integer, nullable=False)
    class_id = Column(String(36), ForeignKey("classes.id", ondelete="SET NULL"), nullable=True)
    student_id = Column(String(36), ForeignKey("students.id", ondelete="SET NULL"), nullable=True)
    assignment_id = Column(String(36), ForeignKey("assignments.id", ondelete="SET NULL"), nullable=True)
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))


class Submission(Base):
    __tablename__ = "submissions"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    key = Column(String(64), ForeignKey("submission_keys.key", ondelete="CASCADE"), nullable=False)
    counter = Column(Integer, nullable=False)  # 1, 2, 3… per key
    project_id = Column(String(36), nullable=True)  # snapshot source
    project_title = Column(String(255), nullable=True)
    zip_path = Column(String(512), nullable=False)
    commit_hash = Column(String(64), nullable=True)  # git commit snapshotting the submission
    submitted_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))


class Marking(Base):
    __tablename__ = "markings"

    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    assignment_id = Column(String(36), ForeignKey("assignments.id", ondelete="CASCADE"), nullable=False)
    student_id = Column(String(36), ForeignKey("students.id", ondelete="CASCADE"), nullable=False)
    project_id = Column(String(36), ForeignKey("snippets.id", ondelete="SET NULL"), nullable=True)
    graded = Column(Boolean, default=False, nullable=False)
    graded_at = Column(DateTime(timezone=True), nullable=True)
    score = Column(String(32), nullable=True)
    feedback_file = Column(Text, default="feedback.md", nullable=False)
    updated_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc), onupdate=lambda: datetime.now(timezone.utc))

    __table_args__ = (UniqueConstraint("assignment_id", "student_id", name="uq_marking_asg_student"),)


class Setting(Base):
    __tablename__ = "settings"
    key = Column(String(64), primary_key=True)
    value = Column(Text, nullable=True)
